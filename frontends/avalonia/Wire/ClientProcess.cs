using System.Diagnostics;
using System.Text;

namespace Gongga.Avalonia.Wire;

public sealed record ClientOptions(
    string? Executable,
    string? Server,
    string? Home,
    string? ClientHome,
    string? Key,
    bool ClientOnly,
    bool Force);

/// The host of the one client child this window owns.
/// Contract: crates/tui/src/pipe.rs -- one UI spawns one client; closing stdin is
/// the window leaving, and an ended stdout is the client leaving.
public sealed class ClientProcess : IDisposable
{
    private readonly object _writeLock = new();
    private readonly List<Task> _readers = new();
    private Process? _process;
    private StreamWriter? _input;
    private bool _stopped;

    public bool IsRunning => _process is { HasExited: false };

    public Task Exited => _process?.WaitForExitAsync() ?? Task.CompletedTask;

    /// Raised once the child's stdout ends: the client leaving is the window leaving.
    public Action? Ended { get; set; }

    /// Whether any frame ever arrived. A child that dies before its first Layout
    /// never connected, and its reason belongs on screen instead of a window
    /// that closes itself.
    public bool SawLayout { get; private set; }

    public string? LogPath { get; private set; }

    /// The child's last diagnostic, for a window that has nothing else to show.
    public string ReadLogTail()
    {
        if (LogPath is null)
        {
            return string.Empty;
        }
        try
        {
            return File.ReadLines(LogPath).LastOrDefault() ?? string.Empty;
        }
        catch (IOException)
        {
            return string.Empty;
        }
        catch (UnauthorizedAccessException)
        {
            return string.Empty;
        }
    }

    /// Returns null on success, or the reason the child could not be started.
    public string? Start(ClientOptions options, Action<RenderFrame> onFrame)
    {
        var executable = ResolveExecutable(options.Executable);
        if (executable is null)
        {
            return "gongga-client.exe was not found beside this program; pass --client <path>";
        }

        var home = options.ClientHome
            ?? Path.Combine(
                Environment.GetFolderPath(Environment.SpecialFolder.UserProfile),
                ".gongga-client");

        var start = new ProcessStartInfo
        {
            FileName = executable,
            UseShellExecute = false,
            CreateNoWindow = true,
            RedirectStandardInput = true,
            RedirectStandardOutput = true,
            RedirectStandardError = true,
            StandardInputEncoding = new UTF8Encoding(false),
            StandardOutputEncoding = Encoding.UTF8,
            StandardErrorEncoding = Encoding.UTF8,
        };
        start.ArgumentList.Add("--parent");
        start.ArgumentList.Add(Environment.ProcessId.ToString());
        AddValue(start, "--server", options.Server);
        AddValue(start, "--home", options.Home);
        AddValue(start, "--client-home", options.ClientHome);
        AddValue(start, "--key", options.Key);
        if (options.ClientOnly)
        {
            start.ArgumentList.Add("--client_only");
        }
        if (options.Force)
        {
            start.ArgumentList.Add("--force");
        }

        try
        {
            var child = Process.Start(start);
            if (child is null)
            {
                return "the operating system did not start gongga-client";
            }
            _process = child;
            _input = new StreamWriter(child.StandardInput.BaseStream, new UTF8Encoding(false))
            {
                AutoFlush = true,
            };
            LogPath = Path.Combine(home, "client.log");
            var frames = Task.Run(() => ReadFrames(child.StandardOutput, frame =>
            {
                if (frame is RenderFrame.Layout)
                {
                    SawLayout = true;
                }
                onFrame(frame);
            }));
            var diagnostics = Task.Run(() => ReadDiagnostics(child.StandardError, LogPath));
            _readers.Add(frames);
            _readers.Add(diagnostics);
            // Both ends are done: the child is gone and its last words are on disk.
            _ = Task.WhenAll(frames, diagnostics)
                .ContinueWith(_ => Ended?.Invoke(), TaskScheduler.Default);
            return null;
        }
        catch (Exception error) when (error is System.ComponentModel.Win32Exception || error is IOException)
        {
            return $"starting gongga-client failed: {error.Message}";
        }
    }

    public void Send(string jsonLine)
    {
        lock (_writeLock)
        {
            if (_stopped || _input is null || _process is { HasExited: true })
            {
                return;
            }
            try
            {
                _input.Write(jsonLine);
                _input.Flush();
            }
            catch (IOException)
            {
                // EOF on stdin is the client leaving; the reader task reports it.
            }
        }
    }

    public void Stop()
    {
        if (_stopped)
        {
            return;
        }
        _stopped = true;
        lock (_writeLock)
        {
            try
            {
                _input?.Close();
            }
            catch (IOException)
            {
            }
            _input = null;
        }
        var child = _process;
        if (child is null)
        {
            return;
        }
        try
        {
            if (!child.WaitForExit(2000))
            {
                child.Kill(entireProcessTree: false);
            }
        }
        catch (InvalidOperationException)
        {
        }
        catch (System.ComponentModel.Win32Exception)
        {
        }
    }

    public void Dispose() => Stop();

    private static void AddValue(ProcessStartInfo start, string flag, string? value)
    {
        if (string.IsNullOrEmpty(value))
        {
            return;
        }
        start.ArgumentList.Add(flag);
        start.ArgumentList.Add(value);
    }

    /// A bundle puts the window and the client beside each other; a development
    /// build has the window under `bin/Debug/net10.0` and the client under the
    /// repository's `target`. Both are found without an argument.
    private static string? ResolveExecutable(string? explicitPath)
    {
        if (!string.IsNullOrEmpty(explicitPath))
        {
            return File.Exists(explicitPath) ? explicitPath : null;
        }
        var fromEnvironment = Environment.GetEnvironmentVariable("GONGGA_CLIENT_EXE");
        if (!string.IsNullOrEmpty(fromEnvironment) && File.Exists(fromEnvironment))
        {
            return fromEnvironment;
        }

        var name = OperatingSystem.IsWindows() ? "gongga-client.exe" : "gongga-client";
        var beside = Path.Combine(AppContext.BaseDirectory, name);
        if (File.Exists(beside))
        {
            return beside;
        }

        for (var current = new DirectoryInfo(AppContext.BaseDirectory);
             current is not null;
             current = current.Parent)
        {
            foreach (var profile in new[] { "debug", "release" })
            {
                var candidate = Path.Combine(current.FullName, "target", profile, name);
                if (File.Exists(candidate))
                {
                    return candidate;
                }
            }
        }
        return null;
    }

    private static async Task ReadFrames(StreamReader output, Action<RenderFrame> onFrame)
    {
        try
        {
            while (await output.ReadLineAsync().ConfigureAwait(false) is { } line)
            {
                if (line.Length == 0)
                {
                    continue;
                }
                var frame = WireReader.Parse(line);
                if (frame is not null)
                {
                    onFrame(frame);
                }
            }
        }
        catch (IOException)
        {
        }
        catch (ObjectDisposedException)
        {
        }
    }

    private static async Task ReadDiagnostics(StreamReader error, string log)
    {
        try
        {
            Directory.CreateDirectory(Path.GetDirectoryName(log)!);
            while (await error.ReadLineAsync().ConfigureAwait(false) is { } line)
            {
                File.AppendAllText(log, line + Environment.NewLine, Encoding.UTF8);
            }
        }
        catch (IOException)
        {
        }
        catch (UnauthorizedAccessException)
        {
        }
        catch (ObjectDisposedException)
        {
        }
    }
}
