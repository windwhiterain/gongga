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
            _readers.Add(Task.Run(() => ReadFrames(child.StandardOutput, onFrame, () => Ended?.Invoke())));
            _readers.Add(Task.Run(() => ReadDiagnostics(child.StandardError, home)));
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
        var own = Environment.ProcessPath;
        if (own is null)
        {
            return null;
        }
        var name = OperatingSystem.IsWindows() ? "gongga-client.exe" : "gongga-client";
        var beside = Path.Combine(Path.GetDirectoryName(own)!, name);
        return File.Exists(beside) ? beside : null;
    }

    private static async Task ReadFrames(
        StreamReader output,
        Action<RenderFrame> onFrame,
        Action onEnd)
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
        finally
        {
            onEnd();
        }
    }

    private static async Task ReadDiagnostics(StreamReader error, string clientHome)
    {
        try
        {
            Directory.CreateDirectory(clientHome);
            var log = Path.Combine(clientHome, "client.log");
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
