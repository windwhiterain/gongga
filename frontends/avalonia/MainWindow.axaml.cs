using Avalonia;
using Avalonia.Controls;
using Avalonia.Input;
using Avalonia.Interactivity;
using Avalonia.Threading;
using Avalonia.VisualTree;
using Gongga.Avalonia.ViewModels;
using Gongga.Avalonia.Wire;

namespace Gongga.Avalonia;

public partial class MainWindow : Window
{
    /// One line of a block: 13pt monospace on a 17px line, plus the template's
    /// 7px vertical padding. The client asks in text rows, so a pane's capacity is
    /// its viewport divided by this.
    private const double PaneRowHeight = 30;

    /// The strip a pane draws its session name in, above the rows.
    private const double PaneHeaderHeight = 26;

    private readonly ClientProcess _client = new();
    private readonly ClientModel _model = new();
    private readonly DispatcherTimer _toastTimer;
    private readonly Dictionary<string, ScrollViewer> _viewers = new();
    private readonly HashSet<string> _olderAsked = new();
    private readonly HashSet<string> _follow = new();
    private readonly Dictionary<string, double> _seenBottom = new();
    private List<(string Window, int Rows)> _rooms = new();
    private List<(string Window, int Columns)> _columns = new();

    /// The widest statement a pane's rows have been proven to fit in, and the box it
    /// was proven in: a column count is a fact about one box width, not about a pane.
    private readonly Dictionary<string, (double Box, int Columns)> _proven = new();

    /// A block's own horizontal padding, in pixels.
    private const double BlockPadding = 12;

    /// One row drawn by the body's own control, laid out to find the advance of a
    /// character: measured on anything else it is a column of error, and every column
    /// of it is a word broken at the pane's edge.
    private readonly Views.SpanLine _ruler = new()
    {
        Spans = [new SpanItem("0123456789", "text")],
        FontFamily = new global::Avalonia.Media.FontFamily("Consolas, Cascadia Mono, monospace"),
        FontSize = 13,
    };
    private bool _applying;

    /// The child this window drives, for a run that has to watch the wire.
    internal ClientProcess Client => _client;

    public MainWindow()
    {
        InitializeComponent();
        var options = Launch.Options.Client;
        DataContext = _model;
        _model.Send = _client.Send;
        _model.ScrollToNewest = ScrollStickyToNewest;
        _model.ReplaceInput = ApplyInput;
        // Tunnel, not bubble: an `AcceptsReturn` TextBox puts the newline in before a
        // bubbling handler is asked, so Enter never reached the submit path.
        Input.AddHandler(KeyDownEvent, OnInputKeyDown, RoutingStrategies.Tunnel);
        // Panes open, split and resize without a frame saying so, and the rows one
        // can draw are a fact about layout: read after every pass, sent when it moves.
        LayoutUpdated += (_, _) =>
        {
            NotifyRooms();
            FollowNewest();
        };

        _toastTimer = new DispatcherTimer { Interval = TimeSpan.FromSeconds(4) };
        _toastTimer.Tick += (_, _) =>
        {
            _model.Toast = string.Empty;
            _toastTimer.Stop();
        };

        _client.Ended = () => Dispatcher.UIThread.Post(() =>
        {
            if (_client.SawLayout)
            {
                if (IsVisible)
                {
                    Close();
                }
                return;
            }
            // The child never reached a frame: the reason is the last thing it said,
            // and a window that closed itself would hide it.
            var reason = _client.ReadLogTail();
            _model.Apply(new RenderFrame.Note(
                LineKind.Error,
                reason.Length == 0
                    ? $"gongga-client exited before it connected; see {_client.LogPath ?? "its log"}"
                    : reason));
        });

        var error = _client.Start(options, frame => Dispatcher.UIThread.Post(() =>
        {
            // Measured before the frame lands: a page is inserted above the rows the
            // pane already holds, and a reader who was not at the oldest row has to
            // stay on the rows they were reading.
            var extents = frame is RenderFrame.Older ? Extents() : null;
            _model.Apply(frame);
            if (extents is not null)
            {
                KeepPlace(extents);
            }
            if (frame is RenderFrame.Stopped)
            {
                _toastTimer.Stop();
                _toastTimer.Start();
            }
        }));
        if (error is not null)
        {
            _model.Apply(new RenderFrame.Note(LineKind.Error, error));
        }
    }

    protected override void OnOpened(EventArgs e)
    {
        base.OnOpened(e);
        if (Screens.Primary is not { } screen)
        {
            return;
        }
        // Bounds are in physical pixels and the window's size is in DIPs, so the
        // screen has to be divided back by its scaling before it can cap a window.
        var area = screen.WorkingArea;
        var scale = screen.Scaling <= 0 ? 1 : screen.Scaling;
        Width = Math.Min(Width, area.Width / scale);
        Height = Math.Min(Height, area.Height / scale);
        Position = new PixelPoint(
            area.X + (int)Math.Max(0, (area.Width - Width * scale) / 2),
            area.Y + (int)Math.Max(0, (area.Height - Height * scale) / 2));
    }

    private void OnClosed(object? sender, EventArgs e)
    {
        _toastTimer.Stop();
        _client.Send(Intents.Quit());
        _client.Stop();
    }

    /// Writes text the process chose into the box, caret included, without asking
    /// the client for candidates a second time: the choice already did.
    private void ApplyInput(string text, int caret)
    {
        _applying = true;
        try
        {
            Input.Text = text;
            Input.CaretIndex = Math.Clamp(caret, 0, text.Length);
        }
        finally
        {
            _applying = false;
        }
    }

    private void OnInputTextChanged(object? sender, TextChangedEventArgs e)
    {
        if (_applying)
        {
            return;
        }
        _model.InputChanged(Input.Text ?? string.Empty, Input.CaretIndex);
    }

    private void OnInputKeyDown(object? sender, KeyEventArgs e)
    {
        var shift = e.KeyModifiers.HasFlag(KeyModifiers.Shift);
        var control = e.KeyModifiers.HasFlag(KeyModifiers.Control);
        var alt = e.KeyModifiers.HasFlag(KeyModifiers.Alt);

        switch (e.Key)
        {
            case Key.Enter when !shift && !alt:
                e.Handled = true;
                _model.Submit();
                break;

            case Key.Tab:
                e.Handled = true;
                if (_model.CompletionVisible)
                {
                    _model.AcceptCompletion();
                }
                else
                {
                    _model.InputChanged(Input.Text ?? string.Empty, Input.CaretIndex);
                }
                break;

            case Key.Escape:
                e.Handled = true;
                _model.CloseCompletion();
                break;

            case Key.Up:
                e.Handled = true;
                if (_model.CompletionVisible)
                {
                    _model.MoveCompletion(-1);
                }
                else if (shift)
                {
                    _model.RecallHistory(-1);
                }
                break;

            // SPEC's `Ctrl+Down: move sticky session to bottom`, told apart before the
            // bare arrow: the popup and the caret are the plain key's business.
            // Contract: crates/tui/src/lib.rs arrow_key.
            case Key.Down when control:
                e.Handled = true;
                _follow.Add(_model.Sticky);
                ScrollStickyToNewest();
                break;

            case Key.Down:
                e.Handled = true;
                if (_model.CompletionVisible)
                {
                    _model.MoveCompletion(1);
                }
                else if (shift)
                {
                    _model.RecallHistory(1);
                }
                break;

            case Key.Q when control:
                e.Handled = true;
                _model.Quit();
                Close();
                break;

            case Key.Delete when shift && (Input.Text?.Length ?? 0) == 0:
                e.Handled = true;
                _model.DeleteStickySession();
                break;

            case Key.PageUp:
                e.Handled = true;
                ScrollSticky(-1);
                break;

            case Key.PageDown:
                e.Handled = true;
                ScrollSticky(1);
                break;
        }
    }

    private void OnBlockPressed(object? sender, PointerPressedEventArgs e)
    {
        if (sender is Border { DataContext: BlockRow block } && block.Foldable)
        {
            block.Toggle();
        }
    }

    private void OnPaneHeaderPressed(object? sender, PointerPressedEventArgs e)
    {
        if (sender is Border { Tag: string name })
        {
            _model.ClickPaneHeader(name);
        }
    }

    /// A group's chevron folds the rows under it, and a name that only groups has
    /// nothing else to do. Contract: crates/tui/src/tree.rs and SPEC's `session` row.
    private void OnFoldPressed(object? sender, PointerPressedEventArgs e)
    {
        if (sender is TextBlock { Tag: string name })
        {
            e.Handled = true;
            _model.ToggleFold(name);
        }
    }

    private void OnSessionPressed(object? sender, PointerPressedEventArgs e)
    {
        if (sender is not Border { Tag: string name })
        {
            return;
        }
        // A row that names no session is a group: opening a window for it would be a
        // name the server does not have.
        if (_model.Sessions.FirstOrDefault(row => row.Name == name) is not { Session: true })
        {
            _model.ToggleFold(name);
            return;
        }
        var toggle = e.KeyModifiers.HasFlag(KeyModifiers.Shift)
            || e.KeyModifiers.HasFlag(KeyModifiers.Control);
        if (!_model.IsShown(name))
        {
            // A pane about to open reports its capacity first. The client sizes the
            // first refill by it and never asks again once the window is in sync, so
            // a capacity reported after the pane is drawn arrives too late.
            _client.Send(Intents.Room([(name, PaneCapacity())]));
        }
        _model.ClickSession(name, toggle);
    }

    private static bool SameRooms(List<(string, int)> left, List<(string, int)> right)
    {
        if (left.Count != right.Count)
        {
            return false;
        }
        for (var index = 0; index < left.Count; index++)
        {
            if (left[index].Item1 != right[index].Item1
                || Math.Abs(left[index].Item2 - right[index].Item2) >= 2)
            {
                return false;
            }
        }
        return true;
    }

    /// How many text rows one pane can draw before it has to scroll.
    private int PaneCapacity()
    {
        // Panes split into columns, so each keeps the area's height: the header
        // strip is what a row does not get.
        var height = PaneArea.Bounds.Height - PaneHeaderHeight;
        if (height <= 0 && _viewers.Values.FirstOrDefault(it => it.Viewport.Height > 0) is { } known)
        {
            height = known.Viewport.Height;
        }
        return Math.Max(1, (int)(height / PaneRowHeight));
    }

    private void OnCandidatePressed(object? sender, PointerPressedEventArgs e)
    {
        if (sender is Border { DataContext: CandidateView candidate })
        {
            _model.AcceptCompletionAt(_model.Candidates.IndexOf(candidate));
        }
    }

    private void OnPaneScrollChanged(object? sender, ScrollChangedEventArgs e)
    {
        if (sender is not ScrollViewer { Tag: string name } viewer)
        {
            return;
        }
        var bottom = Math.Max(0, viewer.Extent.Height - viewer.Viewport.Height);
        if (viewer.Offset.Y >= bottom - 2)
        {
            _follow.Add(name);
        }
        else
        {
            _follow.Remove(name);
        }
        // A pane at its oldest row asks once, not on every scroll tick: the latch
        // is released when the operator scrolls away from the top.
        if (viewer.Offset.Y > 0)
        {
            _olderAsked.Remove(name);
            return;
        }
        if (viewer.Extent.Height > viewer.Viewport.Height && _olderAsked.Add(name))
        {
            // The page is the rows this pane can draw: the same count the client
            // sizes its refill by, so what comes back fills the pane.
            _model.RequestOlder(name);
        }
    }

    /// Caches a pane's scroller, then reports the rows it can draw.
    private void OnPaneLoaded(object? sender, RoutedEventArgs e)
    {
        if (sender is not ScrollViewer { Tag: string name } viewer)
        {
            return;
        }
        _viewers[name] = viewer;
        _follow.Add(name);
        NotifyRooms();
        FollowNewest();
    }

    /// How tall each pane's rows were before a frame was applied.
    private Dictionary<string, double> Extents()
    {
        var extents = new Dictionary<string, double>();
        foreach (var (name, viewer) in _viewers)
        {
            extents[name] = viewer.Extent.Height;
        }
        return extents;
    }

    /// Adds what a page grew to its pane's offset, so the rows under the pointer stay
    /// under the pointer. A pane at its newest row is moved to the new bottom by
    /// `FollowNewest` instead.
    private void KeepPlace(Dictionary<string, double> extents)
    {
        foreach (var (name, viewer) in _viewers)
        {
            if (!extents.TryGetValue(name, out var before) || _follow.Contains(name))
            {
                continue;
            }
            var grew = viewer.Extent.Height - before;
            if (grew > 0)
            {
                viewer.Offset = new Vector(viewer.Offset.X, viewer.Offset.Y + grew);
                _seenBottom[name] = Math.Max(0, viewer.Extent.Height - viewer.Viewport.Height);
            }
        }
    }

    /// Keeps a pane at its newest row while it is following.
    ///
    /// # Invariant
    ///
    /// It runs after a layout pass, not when rows are added: a row is measured after
    /// it is added, and an extent read before that is the extent of the old content.
    /// A pane the operator scrolled away from is not in `_follow` and is left alone.
    private void FollowNewest()
    {
        foreach (var (name, viewer) in _viewers)
        {
            var bottom = Math.Max(0, viewer.Extent.Height - viewer.Viewport.Height);
            var previous = _seenBottom.TryGetValue(name, out var seen) ? seen : -1;
            _seenBottom[name] = bottom;
            // Only a pane whose content grew is moved: a scroll of the operator's own
            // leaves the extent alone, and dragging them back would make PageUp useless.
            if (bottom > previous + 1 && _follow.Contains(name))
            {
                viewer.Offset = new Vector(viewer.Offset.X, bottom);
            }
        }
    }

    private void OnPaneUnloaded(object? sender, RoutedEventArgs e)
    {
        if (sender is ScrollViewer { Tag: string name } viewer
            && _viewers.TryGetValue(name, out var known) && known == viewer)
        {
            _viewers.Remove(name);
            NotifyRooms();
        }
    }

    /// Tells the client how many rows each open pane can draw.
    ///
    /// # Invariant
    ///
    /// It is sent only when the answer changes, because the client sizes a refill
    /// page by it: a pane that never reports one gets the client's default of a
    /// single row, and a reopened session then shows one line of its history.
    private void NotifyRooms()
    {
        var rooms = new List<(string, int)>();
        foreach (var pane in _model.Panes)
        {
            if (_viewers.TryGetValue(pane.Name, out var viewer) && viewer.Viewport.Height > 0)
            {
                rooms.Add((pane.Name, Math.Max(1, (int)(viewer.Viewport.Height / PaneRowHeight))));
            }
        }
        // A scrollbar appearing narrows the viewport by a row or two, and rows
        // arriving toggle it: only a real change is worth a line on the pipe.
        if (!SameRooms(rooms, _rooms))
        {
            _rooms = rooms;
            if (rooms.Count != 0)
            {
                _client.Send(Intents.Room(rooms));
            }
        }
        NotifyColumns();
    }

    /// Tells the client how wide each open pane draws a body: the width its document
    /// rows are wrapped at. A body is wrapped once, in the client, so this is the one
    /// measurement only the drawing can make. See `docs/notes/client-document.md`.
    ///
    /// # Invariant
    ///
    /// A row is judged only once the rows in hand were wrapped at the width just
    /// stated: a row still holding an older, wider wrap says nothing about this one.
    private void NotifyColumns()
    {
        var panes = new List<(string Name, double Box, int Columns, List<Views.SpanLine> Rows)>();
        foreach (var pane in _model.Panes)
        {
            if (!_viewers.TryGetValue(pane.Name, out var viewer) || viewer.Viewport.Width <= 0)
            {
                continue;
            }
            // The width a row is actually given: a row is the box its own words are
            // drawn in, so its laid-out width is that width -- measured, not derived
            // from paddings this file would have to know. The pane's viewport is not
            // it: the block's padding, the fold's column, the kind's mark and SPEC's
            // `box_edge` margin all sit between the two, and every guess at them cost
            // prose one column per guess.
            var rows = viewer.GetVisualDescendants()
                .OfType<Views.SpanLine>()
                .Where(line => line.Bounds.Width > 0)
                .ToList();
            var usable = rows.Count != 0
                ? rows[0].Bounds.Width
                : viewer.Viewport.Width - viewer.Padding.Left - viewer.Padding.Right
                    - BlockPadding - 2 * _model.Edge;
            // One column of slack: a row that wraps a character early is a row, one
            // that wraps late is a lost letter.
            var stated = Math.Max(20, (int)(usable / Advance));
            // A width this pane's rows were already too wide for stays narrowed: the
            // estimate is the same number every pass, so without the memory the pass
            // that fits would widen it back and the two would trade places forever.
            if (rows.Count != 0
                && _proven.TryGetValue(pane.Name, out var proven)
                && Math.Abs(proven.Box - usable) < 0.5)
            {
                stated = Math.Min(stated, proven.Columns);
            }
            panes.Add((pane.Name, usable, stated, rows));
        }
        var columns = panes.Select(pane => (pane.Name, pane.Columns)).ToList();
        if (!SameColumns(columns, _columns))
        {
            _columns = columns;
            if (columns.Count != 0)
            {
                _client.Send(Intents.Columns(columns));
            }
            return;
        }
        // A glyph the body's face does not have is drawn from a fallback at another
        // advance, so a row that fits by column count can still be a pixel too wide
        // and take a second line. A document row is one line, so the rows themselves
        // say which pane is over, and one column comes off it.
        //
        // A row holding more characters than the width states was wrapped at an older
        // width and says nothing about this one: the client answers a stated width one
        // pass later, and judging before it does narrows a pane column by column for
        // as long as the check keeps looking at the row before the last.
        var narrowed = panes
            .Where(pane => pane.Rows.Exists(row => row.TextLayout.TextLines.Count > 1
                && Characters(row) <= pane.Columns))
            .Where(pane => pane.Columns > 20)
            .ToList();
        if (narrowed.Count == 0)
        {
            return;
        }
        foreach (var pane in narrowed)
        {
            _proven[pane.Name] = (pane.Box, pane.Columns - 1);
        }
        var next = panes
            .Select(pane => (pane.Name, pane.Columns - (narrowed.Exists(one => one.Name == pane.Name) ? 1 : 0)))
            .ToList();
        _columns = next;
        _client.Send(Intents.Columns(next));
    }

    /// How wide one character of a body is, in pixels.
    private double Advance
    {
        get
        {
            _ruler.Measure(global::Avalonia.Size.Infinity);
            var width = _ruler.DesiredSize.Width / 10;
            return width > 1 ? width : 8;
        }
    }

    /// How many characters a row carries.
    ///
    /// # Invariant
    ///
    /// A row never holds more characters than the width it was wrapped at, so this is
    /// how a row still holding an older, wider wrap is told from one drawn too wide.
    private static int Characters(Views.SpanLine row) =>
        row.Spans?.Sum(span => span.Text.Length) ?? 0;

    private static bool SameColumns(List<(string, int)> left, List<(string, int)> right)
    {
        if (left.Count != right.Count)
        {
            return false;
        }
        for (var index = 0; index < left.Count; index++)
        {
            if (left[index].Item1 != right[index].Item1 || left[index].Item2 != right[index].Item2)
            {
                return false;
            }
        }
        return true;
    }

    private ScrollViewer? StickyViewer() => this.GetVisualDescendants()
        .OfType<ScrollViewer>()
        .FirstOrDefault(viewer => Equals(viewer.Tag, _model.Sticky));

    private void ScrollStickyToNewest()
    {
        if (StickyViewer() is { } viewer)
        {
            viewer.Offset = new Vector(
                viewer.Offset.X,
                Math.Max(0, viewer.Extent.Height - viewer.Viewport.Height));
        }
    }

    private void ScrollSticky(int direction)
    {
        if (StickyViewer() is not { } viewer)
        {
            return;
        }
        var bottom = Math.Max(0, viewer.Extent.Height - viewer.Viewport.Height);
        var target = Math.Clamp(
            viewer.Offset.Y + direction * viewer.Viewport.Height * 0.9,
            0,
            bottom);
        viewer.Offset = new Vector(viewer.Offset.X, target);
    }
}
