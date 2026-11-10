using Avalonia;
using Avalonia.Controls;
using Avalonia.Headless;
using Avalonia.Input;
using Avalonia.Media.Imaging;
using Avalonia.Threading;
using Avalonia.VisualTree;
using Gongga.Avalonia.ViewModels;

namespace Gongga.Avalonia.Wire;

/// Renders the real window off screen and writes PNGs, so a layout can be looked at
/// without putting a window on anyone's desktop.
///
/// # Invariant
///
/// It drives the same `MainWindow`, the same `ClientProcess` and the same frames as
/// the interactive entry: a screenshot here is the pixels that entry would draw. It
/// also stays on the thread that set the platform up: a `Dispatcher.Invoke` from
/// that thread waits on the job it just queued, which is a deadlock, not a wait.
public static class ShotRun
{
    /// How long the window is watched before the last frame is taken.
    private static readonly TimeSpan Settle = TimeSpan.FromSeconds(3);

    public static int Run(ClientOptions client, string directory)
    {
        _ = client;
        Directory.CreateDirectory(directory);
        _ = AppBuilder.Configure<App>()
            .UseHeadless(new AvaloniaHeadlessPlatformOptions { UseHeadlessDrawing = false })
            .UseSkia()
            .SetupWithoutStarting();

        MainWindow window;
        try
        {
            window = new MainWindow { Width = 1400, Height = 900 };
            window.Show();
            Pump();
        }
        catch (Exception error)
        {
            Console.Error.WriteLine(error);
            return 1;
        }

        Capture(window, Path.Combine(directory, "01-before-frames.png"));

        var until = DateTime.UtcNow + Settle;
        while (DateTime.UtcNow < until)
        {
            Pump();
            Thread.Sleep(50);
        }
        Capture(window, Path.Combine(directory, "02-connected.png"));

        // What a person would see: the control tree with the box each control got.
        File.WriteAllText(Path.Combine(directory, "tree.txt"), Describe(window));

        // A session in the list, clicked the way a person clicks it.
        if (Session(window, "beta") is not { } row || !Click(window, row))
        {
            // A missing row is a missing picture, and a run that quietly skips a
            // step reports on a window nobody looked at.
            Console.Error.WriteLine("no session row named beta to click");
            window.Close();
            return 1;
        }
        SettleOn(window);
        Capture(window, Path.Combine(directory, "03-session-clicked.png"));
        File.WriteAllText(Path.Combine(directory, "tree-clicked.txt"), Describe(window));

        if (window.FindControl<TextBox>("Input") is { } input)
        {
            input.Focus();
            Pump();
            window.KeyTextInput("/set_re");
            SettleOn(window);
            Capture(window, Path.Combine(directory, "04-completion.png"));
            File.WriteAllText(Path.Combine(directory, "tree-completion.txt"), Describe(window));

            // Tab takes the highlighted candidate and asks again for the rest.
            window.KeyPress(Key.Tab, RawInputModifiers.None, PhysicalKey.Tab, null);
            SettleOn(window);
            Capture(window, Path.Combine(directory, "05-after-tab.png"));

            // A whole command typed out, then Enter: the round trip, not just the box.
            input.Text = string.Empty;
            Pump();
            window.KeyTextInput("/set_session --id delta --model flash-fake");
            window.KeyPress(Key.Enter, RawInputModifiers.None, PhysicalKey.Enter, null);
            SettleOn(window);
            Capture(window, Path.Combine(directory, "06-submitted.png"));
            File.WriteAllText(Path.Combine(directory, "tree-submitted.txt"), Describe(window));
        }

        window.Close();
        Pump();
        return 0;
    }

    /// Waits long enough for frames the client sends in reply to arrive and draw.
    private static void SettleOn(Window window)
    {
        var until = DateTime.UtcNow + TimeSpan.FromSeconds(1);
        while (DateTime.UtcNow < until)
        {
            Pump();
            Thread.Sleep(40);
        }
        // The frame's own dispatch is posted, so one more turn is owed to it.
        Pump();
    }

    /// The listed row whose view model names this session.
    private static Border? Session(Window window, string name) =>
        window.GetVisualDescendants()
            .OfType<Border>()
            .FirstOrDefault(item => item.Tag is string tag && tag == name
                && item.DataContext is SessionRow row && row.Name == name);

    /// Presses the middle of a control, in window coordinates.
    private static bool Click(Window window, Control control)
    {
        if (control.TranslatePoint(new Point(0, 0), window) is not { } origin)
        {
            return false;
        }
        var point = origin + new Point(control.Bounds.Width / 2, control.Bounds.Height / 2);
        window.MouseMove(point);
        window.MouseDown(point, MouseButton.Left);
        window.MouseUp(point, MouseButton.Left);
        Pump();
        return true;
    }

    /// Runs every queued job, so a window that draws on the dispatcher keeps up.
    private static void Pump() => Dispatcher.UIThread.RunJobs();

    private static void Capture(Window window, string path)
    {
        Pump();
        var frame = window.CaptureRenderedFrame();
        if (frame is null)
        {
            Console.Error.WriteLine($"no frame for {path}");
            return;
        }
        frame.Save(path, new PngBitmapEncoderOptions());
        Console.WriteLine($"{Path.GetFileName(path)} {frame.PixelSize.Width}x{frame.PixelSize.Height}");
    }

    /// One line per control: what it is, what box it got, what it says.
    private static string Describe(Window window)
    {
        var lines = new List<string>
        {
            $"window {window.Bounds.Width}x{window.Bounds.Height}",
        };
        Walk(window, window, 0, lines);
        return string.Join(Environment.NewLine, lines);
    }

    private static void Walk(Visual visual, Visual root, int depth, List<string> lines)
    {
        var indent = new string(' ', depth * 2);
        var text = visual switch
        {
            TextBlock block => " \"" + Truncate(block.Text) + "\"",
            TextBox box => " text=\"" + Truncate(box.Text) + "\"",
            _ => string.Empty,
        };
        var visible = visual.IsVisible ? string.Empty : " (hidden)";
        var state = visual is Border { DataContext: SessionRow session }
            ? (session.Shown ? " [shown]" : " [hidden]")
            : string.Empty;
        lines.Add($"{indent}{visual.GetType().Name} {Box(visual, root)}{text}{state}{visible}");
        foreach (var child in visual.GetVisualChildren())
        {
            Walk(child, root, depth + 1, lines);
        }
    }

    private static string Box(Visual visual, Visual root)
    {
        var origin = visual.TranslatePoint(new Point(0, 0), root) ?? new Point(double.NaN, double.NaN);
        return string.Format(
            System.Globalization.CultureInfo.InvariantCulture,
            "@{0:0.#},{1:0.#} {2:0.#}x{3:0.#}",
            origin.X,
            origin.Y,
            visual.Bounds.Width,
            visual.Bounds.Height);
    }

    private static string Truncate(string? text)
    {
        if (string.IsNullOrEmpty(text))
        {
            return string.Empty;
        }
        var single = text.ReplaceLineEndings(" ");
        return single.Length <= 70 ? single : single[..67] + "...";
    }
}
