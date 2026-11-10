using System.Collections.ObjectModel;
using System.Linq;
using Gongga.Avalonia.Wire;

namespace Gongga.Avalonia.ViewModels;

/// The whole view state a stateless pipe front-end has to keep.
/// Contract: crates/tui/src/run.rs apply_frame and crates/tui/src/pipe.rs.
public sealed class ClientModel : ViewModelBase
{
    public const string Help =
        "Enter submit \u00b7 Shift+Enter newline \u00b7 Tab complete \u00b7 PageUp/PageDown scroll \u00b7 " +
        "Shift+\u2191/\u2193 history \u00b7 Shift+Delete delete session \u00b7 Ctrl+Q exit";

    private readonly Dictionary<string, ObservableCollection<BlockRow>> _windows = new();
    private readonly Dictionary<string, BlockRow> _open = new();
    private readonly Dictionary<string, List<BlockRow>> _byKey = new();
    private readonly HashSet<string> _unseen = new();
    private readonly HashSet<string> _olderPending = new();
    private readonly List<string> _history = new();
    private int _historyPosition;
    private WindowLayout _layout = new([], [], string.Empty, null);
    private string _sticky = string.Empty;
    private string _statusText = "starting";
    private LineKind _statusKind = LineKind.System;
    private string _serverText = string.Empty;
    private bool _serverOurs;
    private string _quotaText = string.Empty;
    private LineKind _quotaKind = LineKind.System;
    private string _inputText = string.Empty;
    private int _inputCaret;
    private string _toast = string.Empty;
    private ObservableCollection<BlockRow> _commandBlocks = new();
    private IReadOnlyList<CandidateRow> _candidateRows = [];
    private string _completionSubject = string.Empty;
    private long _completionStart;
    private long _completionEnd;
    private int _selected;
    private bool _completionVisible;

    public ObservableCollection<SessionRow> Sessions { get; } = new();
    public ObservableCollection<PaneRow> Panes { get; } = new();
    public ObservableCollection<CandidateView> Candidates { get; } = new();

    public ClientModel() =>
        // An empty area has to say why it is empty; that is a property of the list.
        Panes.CollectionChanged += (_, _) => Raise(nameof(NoPanes));

    /// Set by the window: writes one JSON line to the child's stdin.
    public Action<string>? Send { get; set; }

    /// Set by the window: brings the sticky pane back to its newest row.
    public Action? ScrollToNewest { get; set; }

    /// Set by the window: writes text and caret into the box the person types in.
    ///
    /// # Invariant
    ///
    /// A text this process chose never lands without its caret: the box keeps its
    /// own caret, so a replacement that forgets it leaves the next keystroke in the
    /// middle of the word.
    public Action<string, int>? ReplaceInput { get; set; }

    /// True while no session has a pane, which is when the area has to explain itself.
    public bool NoPanes => Panes.Count == 0;

    public string StatusText
    {
        get => _statusText;
        private set
        {
            Set(ref _statusText, value, nameof(StatusText));
            Raise(nameof(Header));
        }
    }

    public LineKind StatusKind
    {
        get => _statusKind;
        private set => Set(ref _statusKind, value, nameof(StatusKind));
    }

    public string ServerText
    {
        get => _serverText;
        private set
        {
            Set(ref _serverText, value, nameof(ServerText));
            Raise(nameof(Header));
        }
    }

    public bool ServerOurs
    {
        get => _serverOurs;
        private set => Set(ref _serverOurs, value, nameof(ServerOurs));
    }

    public string Header => ServerText.Length == 0
        ? $"gongga \u00b7 {StatusText}"
        : $"gongga \u00b7 {ServerText} \u00b7 {StatusText}";

    public string QuotaText
    {
        get => _quotaText;
        private set => Set(ref _quotaText, value, nameof(QuotaText));
    }

    public LineKind QuotaKind
    {
        get => _quotaKind;
        private set => Set(ref _quotaKind, value, nameof(QuotaKind));
    }

    public string Toast
    {
        get => _toast;
        set => Set(ref _toast, value, nameof(Toast));
    }

    public string InputText
    {
        get => _inputText;
        set => Set(ref _inputText, value, nameof(InputText));
    }

    public int InputCaret
    {
        get => _inputCaret;
        set => Set(ref _inputCaret, value, nameof(InputCaret));
    }

    public ObservableCollection<BlockRow> CommandBlocks
    {
        get => _commandBlocks;
        private set => Set(ref _commandBlocks, value, nameof(CommandBlocks));
    }

    public bool CompletionVisible
    {
        get => _completionVisible;
        private set => Set(ref _completionVisible, value, nameof(CompletionVisible));
    }

    public string CompletionSubject
    {
        get => _completionSubject;
        private set => Set(ref _completionSubject, value, nameof(CompletionSubject));
    }

    public string Sticky => _sticky;

    public string HelpText => Help;

    public void Apply(RenderFrame frame)
    {
        switch (frame)
        {
            case RenderFrame.Line value:
                Add(new BlockRow(value.Window, string.Empty, value.Kind, string.Empty, value.Text)
                {
                    Sealed = true,
                });
                break;

            case RenderFrame.Stream value:
                ApplyStream(value);
                break;

            case RenderFrame.Status value:
                // Contract: TuiSink::status forces the kind back to System.
                StatusKind = LineKind.System;
                StatusText = value.Text;
                break;

            case RenderFrame.Note value:
                StatusKind = value.Kind;
                StatusText = value.Text;
                break;

            case RenderFrame.Layout value:
                _layout = value.Value;
                RebuildSessions(value.Value);
                RebuildPanes(value.Value);
                CommandBlocks = Window(value.Value.Command);
                QuotaText = value.Value.Quota?.Text ?? string.Empty;
                QuotaKind = value.Value.Quota?.Kind ?? LineKind.System;
                _olderPending.Clear();
                break;

            case RenderFrame.Rewrite value:
                if (FindByKey(value.Key) is { } rewritten)
                {
                    rewritten.Text = value.Text;
                }
                break;

            case RenderFrame.ToolCall value:
                ApplyToolCall(value);
                break;

            case RenderFrame.Relabel value:
                if (FindByKey(value.Key) is { } relabelled)
                {
                    relabelled.Tag = value.Tag;
                    relabelled.Kind = value.Kind;
                }
                break;

            case RenderFrame.Forget value:
                ForgetWindow(value.Window);
                break;

            case RenderFrame.Clear value:
                // Contract: crates/tui/src/log.rs clear keeps the pane, Forget does not.
                Window(value.Window).Clear();
                foreach (var key in _open.Where(pair => pair.Value.Window == value.Window)
                             .Select(pair => pair.Key).ToList())
                {
                    _open.Remove(key);
                }
                break;

            case RenderFrame.StickySession value:
                var name = value.Name ?? string.Empty;
                if (name != _sticky)
                {
                    _sticky = name;
                    foreach (var pane in Panes)
                    {
                        pane.Sticky = pane.Name == name;
                    }
                    ScrollToNewest?.Invoke();
                }
                break;

            case RenderFrame.Stopped value:
                if (!_layout.Shown.Contains(value.Session))
                {
                    _unseen.Add(value.Session);
                    foreach (var row in Sessions)
                    {
                        if (row.Name == value.Session)
                        {
                            row.Unseen = true;
                        }
                    }
                }
                Toast = value.Failed ? "the turn failed" : "the turn ended";
                break;

            case RenderFrame.Draft value:
                CloseCompletion();
                SetInput(value.Text, value.Text.Length);
                break;

            case RenderFrame.Server value:
                ServerText = value.Label ?? string.Empty;
                ServerOurs = value.Ours;
                break;

            case RenderFrame.Repoint:
                // Contract: crates/tui/src/lib.rs drop_windows forgets every window.
                foreach (var blocks in _windows.Values)
                {
                    foreach (var block in blocks)
                    {
                        block.Dropped = true;
                    }
                    blocks.Clear();
                }
                _windows.Clear();
                _open.Clear();
                _byKey.Clear();
                CommandBlocks = Window(_layout.Command);
                break;

            case RenderFrame.Vocabulary:
                Send?.Invoke(Intents.Complete(InputText, InputCaret));
                break;

            case RenderFrame.Completion value:
                ApplyCompletionFrame(value);
                break;
        }
    }

    private void ApplyStream(RenderFrame.Stream value)
    {
        BlockRow? live = null;
        if (_open.TryGetValue(value.Key, out var found))
        {
            live = found;
            if (live.Tag != value.Prefix)
            {
                if (value.Done && value.Prefix.Length != 0)
                {
                    live.Tag = value.Prefix;
                }
                else
                {
                    live.Sealed = true;
                    _open.Remove(value.Key);
                    live = null;
                }
            }
        }
        if (live is null)
        {
            if (value.Text.Length == 0 && value.Prefix.Length == 0)
            {
                return;
            }
            live = new BlockRow(value.Window, value.Key, value.Kind, value.Prefix, value.Text);
            Add(live);
            _open[value.Key] = live;
        }
        else
        {
            live.Text += value.Text;
        }
        if (value.Done)
        {
            live.Sealed = true;
            _open.Remove(value.Key);
        }
    }

    private void ApplyToolCall(RenderFrame.ToolCall value)
    {
        var head = value.Head;
        var block = FindByKey(value.Key);
        if (block is null)
        {
            block = new BlockRow(value.Window, value.Key, LineKind.ToolCall, head.Label, string.Empty);
            Add(block);
        }
        block.Kind = LineKind.ToolCall;
        block.Head = head;
        block.Tag = head.Label;
        block.Text = string.Empty;
        block.Sealed = true;
        _open.Remove(value.Key);
    }

    private void ApplyCompletionFrame(RenderFrame.Completion value)
    {
        _completionStart = value.Start;
        _completionEnd = value.End;
        CompletionSubject = value.Subject ?? string.Empty;
        _candidateRows = value.Candidates;
        Candidates.Clear();
        for (var index = 0; index < value.Candidates.Count; index++)
        {
            var candidate = value.Candidates[index];
            Candidates.Add(new CandidateView(candidate.Insert, candidate.Detail)
            {
                Selected = index == 0,
            });
        }
        _selected = 0;
        CompletionVisible = Candidates.Count != 0;
    }

    private void RebuildSessions(WindowLayout layout)
    {
        for (var index = Sessions.Count - 1; index >= 0; index--)
        {
            if (!layout.Listed.Any(row => row.Name == Sessions[index].Name))
            {
                Sessions.RemoveAt(index);
            }
        }
        for (var wanted = 0; wanted < layout.Listed.Count; wanted++)
        {
            var row = layout.Listed[wanted];
            var existing = Sessions.FirstOrDefault(item => item.Name == row.Name);
            if (existing is null)
            {
                existing = new SessionRow(row.Name);
                Sessions.Add(existing);
            }
            existing.Status = row.Status;
            existing.Title = row.Title;
            existing.Unseen = _unseen.Contains(row.Name);
            // What the list lights up is which windows are open, not where a click
            // last landed: the layout is the only thing that knows.
            existing.Shown = layout.Shown.Contains(row.Name);
            var current = -1;
            for (var scan = 0; scan < Sessions.Count; scan++)
            {
                if (Sessions[scan] == existing)
                {
                    current = scan;
                    break;
                }
            }
            if (current >= 0 && current != wanted)
            {
                Sessions.Move(current, wanted);
            }
        }
        _unseen.RemoveWhere(name => layout.Shown.Contains(name));
    }

    private void RebuildPanes(WindowLayout layout)
    {
        var wanted = layout.Shown.Where(name => name.Length != 0).ToList();
        for (var index = Panes.Count - 1; index >= 0; index--)
        {
            if (!wanted.Contains(Panes[index].Name))
            {
                Panes.RemoveAt(index);
            }
        }
        foreach (var name in wanted)
        {
            var existing = Panes.FirstOrDefault(pane => pane.Name == name);
            if (existing is null)
            {
                existing = new PaneRow(name, Window(name));
                Panes.Add(existing);
            }
            existing.Title = layout.Listed.FirstOrDefault(row => row.Name == name)?.Title ?? string.Empty;
            existing.Sticky = name == _sticky;
        }
        for (var index = 0; index < wanted.Count; index++)
        {
            var current = -1;
            for (var scan = 0; scan < Panes.Count; scan++)
            {
                if (Panes[scan].Name == wanted[index])
                {
                    current = scan;
                    break;
                }
            }
            if (current >= 0 && current != index)
            {
                Panes.Move(current, index);
            }
        }
    }

    private ObservableCollection<BlockRow> Window(string name)
    {
        if (!_windows.TryGetValue(name, out var blocks))
        {
            blocks = new ObservableCollection<BlockRow>();
            _windows[name] = blocks;
        }
        return blocks;
    }

    private void Add(BlockRow block)
    {
        Window(block.Window).Add(block);
        if (block.Key.Length == 0)
        {
            return;
        }
        if (!_byKey.TryGetValue(block.Key, out var list))
        {
            list = new List<BlockRow>();
            _byKey[block.Key] = list;
        }
        list.Add(block);
    }

    private BlockRow? FindByKey(string key)
    {
        if (_open.TryGetValue(key, out var live) && !live.Dropped)
        {
            return live;
        }
        if (!_byKey.TryGetValue(key, out var list))
        {
            return null;
        }
        for (var index = list.Count - 1; index >= 0; index--)
        {
            if (!list[index].Dropped)
            {
                return list[index];
            }
        }
        return null;
    }

    private void ForgetWindow(string name)
    {
        if (_windows.Remove(name, out var blocks))
        {
            foreach (var block in blocks)
            {
                block.Dropped = true;
            }
            blocks.Clear();
        }
        foreach (var key in _open.Where(pair => pair.Value.Window == name)
                     .Select(pair => pair.Key).ToList())
        {
            _open.Remove(key);
        }
        if (name == _layout.Command)
        {
            CommandBlocks = Window(name);
        }
        _unseen.Remove(name);
    }

    /// A program-side change to the input (draft, history, accepted completion).
    ///
    /// # Invariant
    ///
    /// The box is told the caret with the text, because a program-side change is
    /// the one case the box cannot work it out: it is not the thing that moved.
    public void SetInput(string text, int caret)
    {
        InputText = text;
        InputCaret = Math.Clamp(caret, 0, text.Length);
        ReplaceInput?.Invoke(InputText, InputCaret);
        if (InputText.Length != 0)
        {
            Send?.Invoke(Intents.Complete(InputText, InputCaret));
        }
    }

    /// The box reporting what it now holds; its caret is already where it belongs.
    public void InputChanged(string text, int caret)
    {
        InputText = text;
        InputCaret = Math.Clamp(caret, 0, text.Length);
        Send?.Invoke(Intents.Complete(InputText, InputCaret));
    }

    public void Submit()
    {
        var line = InputText.Trim();
        if (line.Length == 0)
        {
            return;
        }
        _history.Add(InputText);
        _historyPosition = _history.Count;
        Send?.Invoke(Intents.Submit(line));
        CloseCompletion();
        SetInput(string.Empty, 0);
        ScrollToNewest?.Invoke();
    }

    public void Quit() => Send?.Invoke(Intents.Quit());

    public void ClickSession(string session, bool toggle)
    {
        var shown = _layout.Shown.Contains(session);
        Send?.Invoke(Intents.RenderSession(session, toggle ? !shown : true, !toggle));
    }

    public void ClickPaneHeader(string session) => Send?.Invoke(Intents.StickySession(session));

    public void DeleteStickySession()
    {
        if (_sticky.Length == 0)
        {
            return;
        }
        Send?.Invoke(Intents.DeleteSession(_sticky));
    }

    public void RequestOlder(string window)
    {
        if (!_olderPending.Add(window))
        {
            return;
        }
        Send?.Invoke(Intents.Older([window]));
    }

    public void MoveCompletion(int delta)
    {
        if (Candidates.Count == 0)
        {
            return;
        }
        Candidates[_selected].Selected = false;
        _selected = (_selected + delta + Candidates.Count) % Candidates.Count;
        Candidates[_selected].Selected = true;
    }

    public void AcceptCompletion() => AcceptCompletionAt(_selected);

    public void AcceptCompletionAt(int index)
    {
        if (!CompletionVisible || index < 0 || index >= _candidateRows.Count)
        {
            return;
        }
        var (text, caret) = Utf8Cursor.ApplyCompletion(
            InputText,
            _completionStart,
            _completionEnd,
            _candidateRows[index]);
        CloseCompletion();
        SetInput(text, caret);
    }

    public void CloseCompletion()
    {
        CompletionVisible = false;
        Candidates.Clear();
        _candidateRows = [];
        _selected = 0;
    }

    public void RecallHistory(int direction)
    {
        if (_history.Count == 0)
        {
            return;
        }
        _historyPosition = Math.Clamp(_historyPosition + direction, 0, _history.Count);
        var text = _historyPosition == _history.Count ? string.Empty : _history[_historyPosition];
        SetInput(text, text.Length);
    }
}
