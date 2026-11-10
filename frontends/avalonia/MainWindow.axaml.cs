using Avalonia;
using Avalonia.Controls;
using Avalonia.Input;
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

    public MainWindow()
    {
        InitializeComponent();
        var options = Launch.Options.Client;
        DataContext = _model;
        _model.Send = _client.Send;
        _model.ScrollToNewest = ScrollStickyToNewest;

        _toastTimer = new DispatcherTimer { Interval = TimeSpan.FromSeconds(4) };
        _toastTimer.Tick += (_, _) =>
        {
            _model.Toast = string.Empty;
            _toastTimer.Stop();
        };

        _client.Ended = () => Dispatcher.UIThread.Post(() =>
        {
            if (IsVisible)
            {
                Close();
            }
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

    private void OnClosed(object? sender, EventArgs e)
    {
        _toastTimer.Stop();
        _client.Send(Intents.Quit());
        _client.Stop();
    }

    private void OnInputTextChanged(object? sender, TextChangedEventArgs e) =>
        _model.InputChanged(Input.Text ?? string.Empty, Input.CaretIndex);

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
