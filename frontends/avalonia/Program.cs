using Avalonia;
using Gongga.Avalonia.Wire;

namespace Gongga.Avalonia;

public static class Launch
{
    public static LaunchOptions Options { get; set; } =
        new(new ClientOptions(null, null, null, null, null, false, false), false);
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

        BuildAvaloniaApp().StartWithClassicDesktopLifetime(Array.Empty<string>());
    }

    public static AppBuilder BuildAvaloniaApp() => AppBuilder.Configure<App>()
        .UsePlatformDetect()
        .WithInterFont()
        .LogToTrace();
}
