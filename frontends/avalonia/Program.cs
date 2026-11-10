using Avalonia;
using Gongga.Avalonia.Wire;

namespace Gongga.Avalonia;

public static class Launch
{
    public static LaunchOptions Options { get; set; } =
        new(new ClientOptions(null, null, null, null, null, false, false), false, null);
}

internal static class Program
{
    [STAThread]
    public static void Main(string[] args)
    {
        var error = LaunchParser.Parse(args, out var options);
        if (error is not null)
        {
            Console.Error.WriteLine(error);
            Environment.ExitCode = 2;
            return;
        }
        Launch.Options = options;

        // The headless entry owns no window, so it must not initialize Avalonia.
        if (options.NoWindow)
        {
            Environment.ExitCode = HeadlessRun.RunAsync(options.Client).GetAwaiter().GetResult();
            return;
        }

        // Off-screen rendering: a real window, pixels written to files, nobody's
        // desktop involved. See `docs/notes/avalonia-frontend.md`.
        if (options.Shots is { } shots)
        {
            Environment.ExitCode = ShotRun.Run(options.Client, shots);
            return;
        }

        BuildAvaloniaApp().StartWithClassicDesktopLifetime(Array.Empty<string>());
    }

    public static AppBuilder BuildAvaloniaApp() => AppBuilder.Configure<App>()
        .UsePlatformDetect()
        .WithInterFont()
        .LogToTrace();
}
