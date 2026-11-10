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
///
/// Every step also records a fact, and a failed fact is a failed run: a picture
/// nobody read is not an observation.
public static class ShotRun
{
    /// How long the window is watched before the first frame is taken.
    private static readonly TimeSpan Settle = TimeSpan.FromSeconds(3);

    private static readonly List<string> Facts = [];
    private static readonly List<string> Wire = [];
    private static readonly List<string> Frames = [];
    private static int _failures;

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

        window.Client.Wrote = line =>
        {
            lock (Wire)
            {
                Wire.Add(line.Trim());
            }
        };
        window.Client.Received = frame =>
        {
            lock (Frames)
            {
                Frames.Add(frame.GetType().Name + " " + Head(frame));
            }
        };
        var model = (ClientModel)window.DataContext!;

        Capture(window, Path.Combine(directory, "01-before-frames.png"));

        // 1. Connect: a Server frame with a label is the only proof the link is up.
        WaitUntil(() => model.Sessions.Count > 0, TimeSpan.FromSeconds(20));
        SettleFor(Settle);
        Capture(window, Path.Combine(directory, "02-connected.png"));
        Fact("connected", model.ServerText.Length > 0, model.ServerText);
        Fact("sessions listed", model.Sessions.Count >= 2, $"{model.Sessions.Count}");

        // 2. A short window, so one page of history is a few rows: a session with
        //    more than that has something above the page to ask for.
        window.Height = 460;
        SettleFor(TimeSpan.FromSeconds(1));
        Capture(window, Path.Combine(directory, "02b-short-window.png"));

        // 3. Open a session. A pane holds the rows the client was told it could hold
        //    first: one block means the capacity never reached the client.
        if (Session(window, "beta") is not { } beta || !Click(window, beta))
        {
            Fact("a session row named beta", false, "not found");
            return Finish(window, directory);
        }
        WaitUntil(() => model.Panes.Count == 1, TimeSpan.FromSeconds(5));
        SettleFor(TimeSpan.FromSeconds(2));
        var viewer = Viewer(window, "beta");
        var opened = Blocks(window, "beta");
        Capture(window, Path.Combine(directory, "03-session-opened.png"));
        Fact("one pane opened", model.Panes.Count == 1, $"{model.Panes.Count}");
        Fact("history refilled", opened >= 5, $"{opened} blocks");
        Fact("capacity reported", Wire.Any(line => line.Contains("\"intent\":\"room\"")),
            Wire.FirstOrDefault(line => line.Contains("\"intent\":\"room\"")) ?? "never sent");
        var shortRoom = RowsFor(Wire.FirstOrDefault(line => line.Contains("\"intent\":\"room\"")), "beta");
        Fact("a new pane opens at its newest row",
            viewer is not null && viewer.Offset.Y >= Bottom(viewer) - 2, Scroller(viewer));

        // 4. PageUp is the gesture for a pane taller than its viewport, and the oldest
        //    row is where a page of the past is asked for.
        var overflows = viewer is not null && viewer.Extent.Height > viewer.Viewport.Height;
        Fact("pane overflows its viewport", overflows, Scroller(viewer));
        if (viewer is not null)
        {
            // Recorded before anything asks for a page: PageUp to the oldest row is
            // itself the gesture, so a "before" taken after it is already an after.
            var firstAtOpen = TextAt(window, "beta", 0);
            var lastAtOpen = TextAt(window, "beta", opened - 1);
            var bottom = viewer.Offset.Y;
            window.FindControl<TextBox>("Input")?.Focus();
            Pump();
            window.KeyPress(Key.PageUp, RawInputModifiers.None, PhysicalKey.PageUp, null);
            SettleFor(TimeSpan.FromSeconds(2));
            var up = viewer.Offset.Y;
            Capture(window, Path.Combine(directory, "04-page-up.png"));
            Fact("PageUp scrolls up", up < bottom - 1, $"{bottom:0.#} -> {up:0.#}");
            Fact("an older page was asked for",
                Wire.Any(line => line.Contains("\"intent\":\"older\"")),
                Wire.LastOrDefault(line => line.Contains("\"intent\":\"older\"")) ?? "never sent");

            var after = Blocks(window, "beta");
            var firstAfter = TextAt(window, "beta", 0);
            Fact("older rows land above the held ones", after > opened && firstAfter != firstAtOpen,
                $"{opened} -> {after} blocks, first \"{firstAtOpen}\" -> \"{firstAfter}\"");
            Fact("the newest row is still the newest",
                TextAt(window, "beta", after - 1) == lastAtOpen,
                $"\"{lastAtOpen}\" -> \"{TextAt(window, "beta", after - 1)}\"");
            Fact("the pane stays where it was scrolled", up < 2, $"{up:0.#}");

            window.KeyPress(Key.PageDown, RawInputModifiers.None, PhysicalKey.PageDown, null);
            SettleFor(TimeSpan.FromSeconds(1));
            Fact("PageDown returns", viewer.Offset.Y > up + 1, $"{up:0.#} -> {viewer.Offset.Y:0.#}");
        }

        // Growing the window asks for a taller page; nothing already drawn is lost.
        window.Height = 900;
        SettleFor(TimeSpan.FromSeconds(1));
        Capture(window, Path.Combine(directory, "05b-tall-window.png"));
        var tall = RowsFor(Wire.LastOrDefault(line => line.Contains("\"intent\":\"room\"")), "beta");
        Fact("a taller window reports a taller page", tall > shortRoom, $"{shortRoom} -> {tall}");

        // 5. Two panes: a control-click adds instead of replacing.
        if (Session(window, "alpha") is { } alpha && Click(window, alpha, RawInputModifiers.Control))
        {
            WaitUntil(() => model.Panes.Count == 2, TimeSpan.FromSeconds(5));
            SettleFor(TimeSpan.FromSeconds(2));
            Capture(window, Path.Combine(directory, "06-two-panes.png"));
            Fact("two panes", model.Panes.Count == 2, $"{model.Panes.Count}");
            Fact("two rows lit", model.Sessions.Count(row => row.Shown) == 2,
                $"{model.Sessions.Count(row => row.Shown)}");

            // 6. A pane's header is the way to type into it.
            if (Header(window, "alpha") is { } header && Click(window, header))
            {
                PumpFor(TimeSpan.FromSeconds(1));
                Capture(window, Path.Combine(directory, "07-sticky-switched.png"));
                Fact("header click moves focus", model.Panes.Count(pane => pane.Sticky) == 1
                    && model.Panes.First(pane => pane.Sticky).Name == "alpha",
                    string.Join(",", model.Panes.Where(pane => pane.Sticky).Select(pane => pane.Name)));
            }
        }

        // 7. A turn that ends: the client says so, the window shows it, then drops it.
        if (window.FindControl<TextBox>("Input") is { } input)
        {
            input.Focus();
            Pump();
            window.KeyTextInput("one turn please");
            window.KeyPress(Key.Enter, RawInputModifiers.None, PhysicalKey.Enter, null);
            var seen = WaitUntil(() => model.Toast.Length > 0, TimeSpan.FromSeconds(20));
            Capture(window, Path.Combine(directory, "08-stopped.png"));
            Fact("a finished turn is announced", seen, model.Toast.Length == 0 ? "no toast" : model.Toast);
            // The toast is dropped by a timer, and a headless dispatcher has no clock
            // to tick it: what is checked is that the row it announced is on screen.
            var alphaViewer = Viewer(window, "alpha");
            Fact("the new row is in view", alphaViewer is null
                || alphaViewer.Offset.Y >= Math.Max(0, alphaViewer.Extent.Height - alphaViewer.Viewport.Height) - 2,
                Scroller(alphaViewer));
        }

        return Finish(window, directory);
    }

    /// Records one checkable claim, and one line of text a person can read.
    private static void Fact(string name, bool ok, string detail)
    {
        if (!ok)
        {
            _failures++;
        }
        var line = $"{(ok ? "PASS" : "FAIL")} {name}{(detail.Length == 0 ? string.Empty : ": " + detail)}";
        Facts.Add(line);
        Console.WriteLine(line);
    }

    /// Something worth saying that is not a claim about this window.
    private static void Note(string text)
    {
        Facts.Add("NOTE " + text);
        Console.WriteLine("NOTE " + text);
    }

    private static int Finish(Window window, string directory)
    {
        File.WriteAllLines(Path.Combine(directory, "facts.txt"), Facts);
        lock (Wire)
        {
            File.WriteAllLines(Path.Combine(directory, "wire.txt"), Wire);
        }
        lock (Frames)
        {
            File.WriteAllLines(Path.Combine(directory, "frames.txt"), Frames);
        }
        File.WriteAllText(Path.Combine(directory, "tree.txt"), Describe(window));
        window.Close();
        Pump();
        return _failures == 0 ? 0 : 1;
    }

    private static bool WaitUntil(Func<bool> done, TimeSpan limit)
    {
        var until = DateTime.UtcNow + limit;
        while (DateTime.UtcNow < until)
        {
            if (done())
            {
                return true;
            }
            Pump();
            Thread.Sleep(50);
        }
        return done();
    }

    private static void SettleFor(TimeSpan period)
    {
        var until = DateTime.UtcNow + period;
        while (DateTime.UtcNow < until)
        {
            Pump();
            Thread.Sleep(40);
        }
        Pump();
    }

    private static void PumpFor(TimeSpan period) => SettleFor(period);

    /// The listed row whose view model names this session.
    private static Border? Session(Window window, string name) =>
        window.GetVisualDescendants()
            .OfType<Border>()
            .FirstOrDefault(item => item.Tag is string tag && tag == name
                && item.DataContext is SessionRow row && row.Name == name);

    /// The strip a pane draws its own name in.
    private static Border? Header(Window window, string name) =>
        window.GetVisualDescendants()
            .OfType<Border>()
            .FirstOrDefault(item => item.Tag is string tag && tag == name
                && item.DataContext is PaneRow row && row.Name == name);

    /// A pane's scroller.
    private static ScrollViewer? Viewer(Window window, string name) =>
        window.GetVisualDescendants()
            .OfType<ScrollViewer>()
            .FirstOrDefault(item => item.Tag is string tag && tag == name);

    /// The text of one block in a pane, in the order they are drawn.
    private static string TextAt(Window window, string name, int index)
    {
        var texts = Viewer(window, name)?.GetVisualDescendants()
            .OfType<SelectableTextBlock>()
            .Select(block => block.Text ?? string.Empty)
            .ToList() ?? [];
        return index >= 0 && index < texts.Count ? texts[index] : "missing";
    }

    /// How many blocks a pane holds: one `SelectableTextBlock` each.
    private static int Blocks(Window window, string name) =>
        Viewer(window, name)?.GetVisualDescendants().OfType<SelectableTextBlock>().Count() ?? 0;

    /// Presses the middle of a control, in window coordinates.
    private static bool Click(Window window, Control control, RawInputModifiers modifiers = default)
    {
        if (control.TranslatePoint(new Point(0, 0), window) is not { } origin)
        {
            return false;
        }
        var point = origin + new Point(control.Bounds.Width / 2, control.Bounds.Height / 2);
        window.MouseMove(point);
        window.MouseDown(point, MouseButton.Left, modifiers);
        window.MouseUp(point, MouseButton.Left, modifiers);
        Pump();
        return true;
    }

    private static void ScrollToEnd(ScrollViewer viewer) =>
        viewer.Offset = new Vector(
            viewer.Offset.X,
            Math.Max(0, viewer.Extent.Height - viewer.Viewport.Height));

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

    /// The rows a room intent claims for one window, or 0 when it does not name it.
    private static int RowsFor(string? line, string window)
    {
        if (line is null)
        {
            return 0;
        }
        var mark = $"\"{window}\",";
        var at = line.IndexOf(mark, StringComparison.Ordinal);
        if (at < 0)
        {
            return 0;
        }
        var rest = line[(at + mark.Length)..];
        var end = rest.IndexOf(']');
        return int.TryParse(rest[..(end < 0 ? rest.Length : end)], out var rows) ? rows : 0;
    }

    private static double Bottom(ScrollViewer viewer) =>
        Math.Max(0, viewer.Extent.Height - viewer.Viewport.Height);

    private static string Scroller(ScrollViewer? viewer) => viewer is null
        ? "no viewer"
        : string.Format(
            System.Globalization.CultureInfo.InvariantCulture,
            "offset {0:0.#} extent {1:0.#} viewport {2:0.#}",
            viewer.Offset.Y,
            viewer.Extent.Height,
            viewer.Viewport.Height);

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

    /// The window or session a frame names, for a log a person can scan.
    private static string Head(RenderFrame frame) => frame switch
    {
        RenderFrame.Line line => line.Window,
        RenderFrame.Stream stream => stream.Window,
        RenderFrame.Clear clear => clear.Window,
        RenderFrame.Forget forget => forget.Window,
        RenderFrame.Stopped stopped => stopped.Session,
        RenderFrame.StickySession sticky => sticky.Name ?? string.Empty,
        _ => string.Empty,
    };

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
