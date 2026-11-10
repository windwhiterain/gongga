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
    private readonly ClientProcess _client = new();
    private readonly ClientModel _model = new();
    private readonly DispatcherTimer _toastTimer;
    private bool _applying;

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
            _model.Apply(frame);
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

    private void OnSessionPressed(object? sender, PointerPressedEventArgs e)
    {
        if (sender is not Border { Tag: string name })
        {
            return;
        }
        var toggle = e.KeyModifiers.HasFlag(KeyModifiers.Shift)
            || e.KeyModifiers.HasFlag(KeyModifiers.Control);
        _model.ClickSession(name, toggle);
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
        if (sender is ScrollViewer { Tag: string name } viewer
            && viewer.Offset.Y <= 0
            && viewer.Extent.Height > viewer.Viewport.Height)
        {
            _model.RequestOlder(name);
        }
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
