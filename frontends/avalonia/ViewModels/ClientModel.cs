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
        "Ctrl+\u2193 newest \u00b7 Shift+\u2191/\u2193 history \u00b7 Shift+Delete delete session \u00b7 Ctrl+Q exit";

    /// The height one row of a block is drawn at, for SPEC's `block_size`: a cap is
    /// a number of rows, and a window measures in pixels.
    public const double LineHeight = 20;

    private readonly Dictionary<string, ObservableCollection<BlockRow>> _windows = new();
    private readonly Dictionary<string, BlockRow> _open = new();
    private readonly Dictionary<string, List<BlockRow>> _byKey = new();
    private readonly HashSet<string> _unseen = new();
    private readonly HashSet<string> _folded = new();
    private readonly List<string> _history = new();
    private int _historyPosition;
    private WindowLayout _layout = new([], [], string.Empty, []);
    private int _blockSize;
    private int _boxEdge = 2;
    private string _sticky = string.Empty;
    private string _statusText = "starting";
    private LineKind _statusKind = LineKind.System;
    private string _serverText = string.Empty;
    private bool _serverOurs;
    private string _quotaText = string.Empty;
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

    public string QuotaText => string.Join("   ", Quota.Select(row => row.Text));

    /// SPEC's `active_quota` block: one row per provider, in the client's order.
    public ObservableCollection<QuotaItem> Quota { get; } = new();

    /// Whether the block is drawn at all: a provider that reported nothing is not a
    /// row of zeroes.
    public bool HasQuota => Quota.Count != 0;

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

    /// The margin a body keeps from the events at each side, in pixels: SPEC's
    /// `box_edge`, which the drawing subtracts before it states a width.
    public double Edge => _boxEdge * BlockRow.EdgeColumn;

    public string HelpText => Help;

    public void Apply(RenderFrame frame)
    {
        switch (frame)
        {
            case RenderFrame.Line value:
                var lined = new BlockRow(value.Window, string.Empty, value.Kind, string.Empty, value.Text)
                {
                    Sealed = true,
                };
                Add(lined);
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
                SyncQuota(value.Value.Quota);
                break;

            case RenderFrame.Older value:
                Prepend(value.Window, value.Rows);
                break;

            case RenderFrame.Document value:
                ApplyDocument(value);
                break;

            case RenderFrame.Render value:
                _blockSize = value.BlockSize;
                _boxEdge = value.BoxEdge;
                ApplyRender();
                break;

            case RenderFrame.Rewind value:
                DropUnsealed(value.Window);
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

    /// A body as finished rows: the block it names is replaced whole.
    ///
    /// # Invariant
    ///
    /// A document is derived from its source, so the rows a window holds are the ones
    /// this frame carried -- never an append, which would leave a row above the last
    /// one saying what the model no longer says.
    private void ApplyDocument(RenderFrame.Document value)
    {
        var block = FindByKey(value.Key);
        if (block is null)
        {
            block = new BlockRow(value.Window, value.Key, value.Kind, value.Tag, string.Empty);
            Add(block);
        }
        block.Kind = value.Kind;
        block.Tag = value.Tag;
        block.Rows = value.Rows.Select(row => new SpanRow(row, value.Kind)).ToList();
        block.Sealed = value.Done;
        if (value.Done)
        {
            _open.Remove(value.Key);
        }
        else
        {
            _open[value.Key] = block;
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
        var wanted = TreeRows(layout);
        var names = wanted.Select(row => row.Name).ToHashSet();
        for (var index = Sessions.Count - 1; index >= 0; index--)
        {
            if (!names.Contains(Sessions[index].Name))
            {
                Sessions.RemoveAt(index);
            }
        }
        for (var place = 0; place < wanted.Count; place++)
        {
            var want = wanted[place];
            var row = Sessions.FirstOrDefault(item => item.Name == want.Name);
            if (row is null)
            {
                row = new SessionRow(want.Name);
                Sessions.Add(row);
            }
            row.Label = want.Label;
            row.Depth = want.Depth;
            row.Session = want.Session;
            row.Group = want.Group;
            row.HasChildren = want.Group;
            row.Collapsed = want.Collapsed;
            row.Mark = want.Mark;
            row.MarkKind = want.MarkKind;
            row.Status = want.Status;
            row.Title = want.Title;
            row.Unseen = want.Unseen;
            // What the list lights up is which windows are open, not where a click
            // last landed: the layout is the only thing that knows.
            row.Shown = want.Shown;
            var current = Sessions.IndexOf(row);
            if (current >= 0 && current != place)
            {
                Sessions.Move(current, place);
            }
        }
        _unseen.RemoveWhere(name => layout.Shown.Contains(name));
    }

    /// SPEC's session list as a tree: every prefix of a name is a row.
    ///
    /// Contract: crates/tui/src/tree.rs rows. Siblings keep the layout's order and a
    /// group stands where its newest session stands, so a pane does not move under
    /// the reader when a turn ends somewhere else.
    private List<SessionRow> TreeRows(WindowLayout layout)
    {
        var nodes = new List<TreeNode>();
        var index = new Dictionary<string, int>();
        var roots = new List<int>();
        for (var place = 0; place < layout.Listed.Count; place++)
        {
            var listed = layout.Listed[place];
            var (mark, kind) = MarkOf(listed, _unseen.Contains(listed.Name));
            var prefix = string.Empty;
            var parent = -1;
            var depth = 0;
            foreach (var segment in listed.Name.Split('/'))
            {
                prefix = depth == 0 ? segment : $"{prefix}/{segment}";
                if (!index.TryGetValue(prefix, out var at))
                {
                    at = nodes.Count;
                    nodes.Add(new TreeNode
                    {
                        Name = prefix,
                        Label = segment,
                        Depth = depth,
                        Latest = place,
                    });
                    index[prefix] = at;
                    if (parent >= 0)
                    {
                        nodes[parent].Children.Add(at);
                    }
                    else
                    {
                        roots.Add(at);
                    }
                }
                nodes[at].Under = Math.Max(nodes[at].Under, mark);
                nodes[at].Latest = Math.Max(nodes[at].Latest, place);
                parent = at;
                depth++;
            }
            if (parent < 0)
            {
                continue;
            }
            nodes[parent].Session = true;
            nodes[parent].Own = mark;
            nodes[parent].Status = listed.Status;
            nodes[parent].Title = listed.Title;
            nodes[parent].Shown = layout.Shown.Contains(listed.Name);
            nodes[parent].Unseen = _unseen.Contains(listed.Name);
        }
        roots.Sort((left, right) => nodes[left].Latest.CompareTo(nodes[right].Latest));
        foreach (var node in nodes)
        {
            node.Children.Sort((left, right) => nodes[left].Latest.CompareTo(nodes[right].Latest));
        }
        var rows = new List<SessionRow>(nodes.Count);
        var stack = new Stack<int>();
        for (var at = roots.Count - 1; at >= 0; at--)
        {
            stack.Push(roots[at]);
        }
        while (stack.Count != 0)
        {
            var node = nodes[stack.Pop()];
            var group = node.Children.Count != 0;
            var collapsed = group && _folded.Contains(node.Name);
            var rank = node.Session ? node.Own : node.Under;
            var (mark, kind) = Symbol(rank);
            rows.Add(new SessionRow(node.Name)
            {
                Label = node.Label,
                Depth = node.Depth,
                Session = node.Session,
                Group = group,
                HasChildren = group,
                Collapsed = collapsed,
                Mark = mark,
                MarkKind = kind,
                Status = node.Status,
                Title = node.Title,
                Shown = node.Shown,
                Unseen = node.Unseen,
            });
            if (collapsed)
            {
                continue;
            }
            for (var at = node.Children.Count - 1; at >= 0; at--)
            {
                stack.Push(node.Children[at]);
            }
        }
        return rows;
    }

    /// The mark of one session: what it is doing, or that it stopped unseen.
    ///
    /// # Invariant
    ///
    /// An error keeps its own mark, because the failure is the stronger fact.
    private static (int Rank, LineKind Kind) MarkOf(ListedRow listed, bool unseen)
    {
        var rank = listed.Status switch
        {
            "running" => 1,
            "error" => 3,
            _ => 0,
        };
        if (rank == 0 && unseen)
        {
            rank = 2;
        }
        return (rank, rank switch
        {
            1 => LineKind.Result,
            2 => LineKind.ToolCall,
            3 => LineKind.Error,
            _ => LineKind.System,
        });
    }

    private static (string Mark, LineKind Kind) Symbol(int rank) => rank switch
    {
        1 => ("\u25cf", LineKind.Result),
        2 => ("\u25cb", LineKind.ToolCall),
        3 => ("\u2715", LineKind.Error),
        _ => ("\u25cb", LineKind.System),
    };

    /// One node of the tree being built, before it is a row.
    private sealed class TreeNode
    {
        public string Name = string.Empty;
        public string Label = string.Empty;
        public string Status = string.Empty;
        public string Title = string.Empty;
        public int Depth;
        public int Latest;
        public bool Session;
        public bool Shown;
        public bool Unseen;
        /// The rank of this node's own session, and of the strongest under it.
        public int Own;
        public int Under;
        public List<int> Children { get; } = new();
    }

    /// Folds or unfolds one group of the list; the client knows nothing of it.
    public void ToggleFold(string name)
    {
        if (!_folded.Remove(name))
        {
            _folded.Add(name);
        }
        RebuildSessions(_layout);
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
            else
            {
                // A `Forget` between two layouts leaves the pane holding a list the
                // model no longer writes to; the layout is when that is noticed.
                existing.Bind(Window(name));
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

    /// Puts a row above the ones already held, at the offset a page fills in order.
    private void Insert(int at, BlockRow block)
    {
        var blocks = Window(block.Window);
        if (at >= blocks.Count)
        {
            Add(block);
            return;
        }
        Dress(block);
        blocks.Insert(at, block);
        Remember(block);
    }

    private void Add(BlockRow block)
    {
        Dress(block);
        Window(block.Window).Add(block);
        Remember(block);
    }

    /// Gives a block the window's own cap and margin, which the frame that sent a
    /// `set_render` applied only to the blocks that existed then.
    private void Dress(BlockRow block)
    {
        block.Cap = _blockSize <= 0
            ? double.PositiveInfinity
            : _blockSize * LineHeight;
        block.Edge = _boxEdge * BlockRow.EdgeColumn;
    }

    private void Remember(BlockRow block)
    {
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

    /// Whether a pane for this session is on screen.
    public bool IsShown(string session) => _layout.Shown.Contains(session);

    public void ClickPaneHeader(string session) => Send?.Invoke(Intents.StickySession(session));

    public void DeleteStickySession()
    {
        if (_sticky.Length == 0)
        {
            return;
        }
        Send?.Invoke(Intents.DeleteSession(_sticky));
    }

    /// Asks for the page above what this pane holds.
    ///
    /// # Invariant
    ///
    /// The answer arrives as one `Older` frame that says it goes above what the pane
    /// holds; nothing here counts rows or watches a clock, so a live row that arrives
    /// while the page is on its way is never mistaken for history.
    public void RequestOlder(string window) => Send?.Invoke(Intents.Older([window]));

    /// Puts one page of the past above the rows this window already holds.
    private void Prepend(string window, IReadOnlyList<PastRow> rows)
    {
        var blocks = Window(window);
        for (var index = 0; index < rows.Count; index++)
        {
            var row = rows[index];
            var block = new BlockRow(window, row.Key, row.Kind, row.Tag, row.Text)
            {
                Sealed = true,
            };
            Dress(block);
            blocks.Insert(index, block);
            Remember(block);
        }
    }

    /// Drops the blocks of one window that had not ended: the tail above the stamp
    /// the client is about to replay. Contract: crates/tui/src/log.rs remove_blocks.
    private void DropUnsealed(string window)
    {
        var blocks = Window(window);
        for (var index = blocks.Count - 1; index >= 0; index--)
        {
            if (blocks[index].Sealed)
            {
                continue;
            }
            blocks[index].Dropped = true;
            blocks.RemoveAt(index);
        }
        foreach (var key in _open.Where(pair => pair.Value.Window == window)
                     .Select(pair => pair.Key).ToList())
        {
            _open.Remove(key);
        }
    }

    /// Writes SPEC's `block_size` and `box_edge` into every block on screen, and into
    /// the ones that arrive later.
    private void ApplyRender()
    {
        var cap = _blockSize <= 0
            ? double.PositiveInfinity
            : _blockSize * LineHeight;
        foreach (var blocks in _windows.Values)
        {
            foreach (var block in blocks)
            {
                block.Cap = cap;
                block.Edge = _boxEdge * BlockRow.EdgeColumn;
            }
        }
    }

    /// Replaces the block with what the client now says it holds.
    private void SyncQuota(IReadOnlyList<QuotaItem> rows)
    {
        Quota.Clear();
        foreach (var row in rows)
        {
            Quota.Add(row);
        }
        Raise(nameof(HasQuota));
        Raise(nameof(QuotaText));
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
