using System.Collections.ObjectModel;
using System.ComponentModel;
using System.Runtime.CompilerServices;
using Avalonia;
using Avalonia.Media;
using Gongga.Avalonia.Wire;

namespace Gongga.Avalonia.ViewModels;

public abstract class ViewModelBase : INotifyPropertyChanged
{
    public event PropertyChangedEventHandler? PropertyChanged;

    protected bool Set<T>(ref T field, T value, [CallerMemberName] string? name = null)
    {
        if (EqualityComparer<T>.Default.Equals(field, value))
        {
            return false;
        }
        field = value;
        Raise(name);
        return true;
    }

    protected void Raise([CallerMemberName] string? name = null) =>
        PropertyChanged?.Invoke(this, new PropertyChangedEventArgs(name));
}

/// One finished row of a document, as a list of classified runs.
///
/// # Invariant
///
/// The rows are the client's and are replaced whole: a document is derived from its
/// source, so one more delta can change a row above the last one.
public sealed class SpanRow
{
    public SpanRow(IReadOnlyList<SpanItem> spans, LineKind kind)
    {
        Spans = spans;
        Kind = kind;
    }

    public IReadOnlyList<SpanItem> Spans { get; }

    /// The kind of the block this row belongs to: what its ink is, never its shape.
    public LineKind Kind { get; }
}

/// One block in a window's log. Contract: crates/tui/src/log.rs Entry.
public sealed class BlockRow : ViewModelBase
{
    /// How wide one of SPEC's `box_edge` columns is drawn, in pixels.
    public const double EdgeColumn = 6;

    private LineKind _kind;
    private string _tag = string.Empty;
    private string _text = string.Empty;
    private bool _sealed;
    private bool _expanded;
    private HeadRow? _head;
    private IReadOnlyList<SpanRow> _rows = [];
    private double _cap = double.PositiveInfinity;
    private double _edge = 2 * EdgeColumn;

    public BlockRow(string window, string key, LineKind kind, string tag, string text)
    {
        Window = window;
        Key = key;
        _kind = kind;
        _tag = tag;
        _text = text;
    }

    public string Window { get; }
    public string Key { get; }
    public bool Dropped { get; set; }

    public LineKind Kind
    {
        get => _kind;
        set
        {
            if (Set(ref _kind, value, nameof(Kind)))
            {
            }
            Raise(nameof(Mark));
            Raise(nameof(Foldable));
            Raise(nameof(DisplayText));
        }
    }

    public string Tag
    {
        get => _tag;
        set
        {
            Set(ref _tag, value, nameof(Tag));
            Raise(nameof(HasTag));
        }
    }

    public bool HasTag => Tag.Length != 0;

    public string Text
    {
        get => _text;
        set
        {
            Set(ref _text, value, nameof(Text));
            Raise(nameof(DisplayText));
        }
    }

    public bool Sealed
    {
        get => _sealed;
        set
        {
            Set(ref _sealed, value, nameof(Sealed));
            Raise(nameof(DisplayText));
        }
    }

    public bool Expanded
    {
        get => _expanded;
        set
        {
            Set(ref _expanded, value, nameof(Expanded));
            Raise(nameof(DisplayText));
        }
    }

    public HeadRow? Head
    {
        get => _head;
        set
        {
            Set(ref _head, value, nameof(Head));
            Raise(nameof(Structured));
            Raise(nameof(DisplayText));
        }
    }

    public bool Structured => Head is not null;

    /// The document this block draws, when the client sent one.
    public IReadOnlyList<SpanRow> Rows
    {
        get => _rows;
        set
        {
            if (Set(ref _rows, value, nameof(Rows)))
            {
                Raise(nameof(HasRows));
            }
        }
    }

    /// Whether the body is rows rather than one string.
    public bool HasRows => _rows.Count != 0;

    /// The cap SPEC's `block_size` states, in pixels; unbounded when it states none.
    public double Cap
    {
        get => _cap;
        set
        {
            if (Set(ref _cap, value, nameof(Cap)))
            {
                Raise(nameof(Capped));
                Raise(nameof(BodyHeight));
                Raise(nameof(FoldMark));
            }
        }
    }

    /// The margin SPEC's `box_edge` gives the events at each side of an opened block.
    public double Edge
    {
        get => _edge;
        set
        {
            if (Set(ref _edge, value, nameof(Edge)))
            {
                Raise(nameof(EdgeMargin));
            }
        }
    }

    /// The margin an opened block keeps from the events at each side.
    public Thickness EdgeMargin => new(_edge, 0, _edge, 0);

    /// Whether this block is drawn inside a capped box: SPEC caps the blocks that
    /// fold, and an answer is not one of them.
    public bool Capped => Foldable && Expanded && !double.IsPositiveInfinity(Cap);

    /// The height the box may take: its cap when it has one, else whatever it needs.
    public double BodyHeight => Capped ? Cap : double.PositiveInfinity;

    /// A fold's own mark: right when there is something inside to open.
    public string FoldMark => !Foldable || !Sealed
        ? string.Empty
        : Expanded ? "\u25be" : "\u25b8";

    public bool Foldable => Kind is LineKind.Reasoning
        or LineKind.ToolCall
        or LineKind.ToolOutput
        or LineKind.ToolError
        or LineKind.Inject;

    // Contract: crates/client/src/lib.rs LineKind::mark.
    public string Mark => Kind switch
    {
        LineKind.Inject => "\u21b3 ",
        LineKind.Command => "$ ",
        LineKind.User => "\u276f ",
        LineKind.Reasoning => "\u25c7 ",
        LineKind.ToolCall => "\u25b8 ",
        LineKind.ToolOutput => "\u2713 ",
        LineKind.ToolError => "\u2715 ",
        LineKind.Result => "\u2192 ",
        LineKind.Error => "! ",
        _ => string.Empty,
    };

    public string Abstract
    {
        get
        {
            if (Head is not null)
            {
                return Head.Summary;
            }
            var first = Text.Replace("\r", string.Empty).Split('\n')[0];
            return first.Length > 120 ? first[..120] + "\u2026" : first;
        }
    }

    /// A sealed folding block shows its abstract until the operator opens it.
    public string DisplayText => Foldable && Sealed && !Expanded ? Abstract : Text;

    public void Toggle() => Expanded = !Expanded;
}

public sealed class PaneRow : ViewModelBase
{
    private ObservableCollection<BlockRow> _blocks;
    private string _title = string.Empty;
    private bool _sticky;

    public PaneRow(string name, ObservableCollection<BlockRow> blocks)
    {
        Name = name;
        _blocks = blocks;
    }

    public string Name { get; }

    /// The rows this pane draws, as the model currently holds them.
    ///
    /// # Invariant
    ///
    /// It is replaced, not captured: `Forget` and `Repoint` drop a window's list and
    /// the next frame makes a new one, so a pane holding the old instance would draw
    /// an empty list for the rest of its life.
    public ObservableCollection<BlockRow> Blocks
    {
        get => _blocks;
        private set => Set(ref _blocks, value, nameof(Blocks));
    }

    public void Bind(ObservableCollection<BlockRow> blocks)
    {
        if (!ReferenceEquals(_blocks, blocks))
        {
            Blocks = blocks;
        }
    }

    public string Title
    {
        get => _title;
        set
        {
            if (Set(ref _title, value, nameof(Title)))
            {
                Raise(nameof(HasTitle));
            }
        }
    }

    /// Whether the strip under a pane's name has anything to say.
    public bool HasTitle => Title.Length != 0;

    public bool Sticky
    {
        get => _sticky;
        set => Set(ref _sticky, value, nameof(Sticky));
    }
}

/// One row of the session list: a session, a group, or a name that is both.
///
/// Contract: crates/tui/src/tree.rs Row -- every prefix of a session's name is a
/// row, a session's own row reports that session, a row that only groups reports
/// the strongest mark below it, and a group's children can be folded away.
public sealed class SessionRow : ViewModelBase
{
    /// A window is open for this session, so typing goes here and the list says so.
    private static readonly IBrush ShownRow = new SolidColorBrush(Color.Parse("#1B2331"));
    private static readonly IBrush HiddenRow = Brushes.Transparent;
    private static readonly IBrush ShownName = new SolidColorBrush(Color.Parse("#E8EEF6"));
    private static readonly IBrush HiddenName = new SolidColorBrush(Color.Parse("#9AA6B6"));

    private string _status = string.Empty;
    private string _title = string.Empty;
    private bool _unseen;
    private bool _shown;
    private bool _collapsed;

    public SessionRow(string name)
    {
        Name = name;
        Label = name[(name.LastIndexOf('/') + 1)..];
        Depth = name.Count(character => character == '/');
    }

    /// The full path this row stands for: a session's own name, or a prefix of one.
    public string Name { get; }

    /// The last segment, which is what the row draws.
    public string Label { get; set; }

    /// How many segments precede [`Label`]; the row is indented two columns each.
    public int Depth { get; set; }

    /// Whether a session is named exactly this, and not only its children.
    public bool Session { get; set; }

    /// Whether any name goes on below this row, so it can be folded.
    public bool Group { get; set; }

    /// The indent the row is drawn at, in pixels.
    public double Indent => 10 + Depth * 14;

    /// A group's own marker: right when its children are folded away.
    public string FoldMark => !Group || Session && !HasChildren ? string.Empty : Collapsed ? "\u203a" : "\u2304";

    /// Whether a fold may be asked for on this row at all.
    public bool HasChildren { get; set; }

    public bool Collapsed
    {
        get => _collapsed;
        set
        {
            if (Set(ref _collapsed, value, nameof(Collapsed)))
            {
                Raise(nameof(FoldMark));
            }
        }
    }

    public string Status
    {
        get => _status;
        set
        {
            Set(ref _status, value, nameof(Status));
            Raise(nameof(Mark));
            Raise(nameof(MarkKind));
        }
    }

    public string Title
    {
        get => _title;
        set
        {
            if (Set(ref _title, value, nameof(Title)))
            {
                Raise(nameof(HasTitle));
            }
        }
    }

    /// Whether the card under the name has anything to say.
    public bool HasTitle => Title.Length != 0;

    /// Whether this session has a window beside the list.
    ///
    /// # Invariant
    ///
    /// It is the `Layout.shown` the client sent, never the row the pointer last hit:
    /// two open windows are two rows saying so, and a click that opened nothing may
    /// not leave one of them lit.
    public bool Shown
    {
        get => _shown;
        set
        {
            if (!Set(ref _shown, value, nameof(Shown)))
            {
                return;
            }
            Raise(nameof(RowBrush));
            Raise(nameof(NameBrush));
            Raise(nameof(BarBrush));
        }
    }

    public IBrush RowBrush => _shown ? ShownRow : HiddenRow;

    /// The bar a shown row carries: the accent, and nothing on a hidden one.
    public IBrush BarBrush => _shown ? Accent : Brushes.Transparent;

    private static readonly IBrush Accent =
        global::Avalonia.Application.Current?.Resources.TryGetResource("AccentBrush", null, out var found) == true
            && found is IBrush accent
            ? accent
            : Brushes.DodgerBlue;

    public IBrush NameBrush => _shown ? ShownName : HiddenName;

    public bool Unseen
    {
        get => _unseen;
        set
        {
            Set(ref _unseen, value, nameof(Unseen));
            Raise(nameof(Mark));
            Raise(nameof(MarkKind));
        }
    }

    /// The one column of the list that says what a session is doing.
    ///
    /// Contract: crates/tui/src/tree.rs Mark -- a session's own state, or the
    /// strongest state of anything under a group.
    private string _mark = "\u25cb";
    private LineKind _markKind = LineKind.System;

    public string Mark
    {
        get => _mark;
        set => Set(ref _mark, value, nameof(Mark));
    }

    public LineKind MarkKind
    {
        get => _markKind;
        set => Set(ref _markKind, value, nameof(MarkKind));
    }
}

public sealed class CandidateView : ViewModelBase
{
    private bool _selected;

    public CandidateView(string insert, string detail)
    {
        Insert = insert;
        Detail = detail;
    }

    public string Insert { get; }
    public string Detail { get; }

    public bool Selected
    {
        get => _selected;
        set => Set(ref _selected, value, nameof(Selected));
    }
}
