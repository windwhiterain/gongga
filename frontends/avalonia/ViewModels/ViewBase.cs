using System.Collections.ObjectModel;
using System.ComponentModel;
using System.Runtime.CompilerServices;
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

/// One block in a window's log. Contract: crates/tui/src/log.rs Entry.
public sealed class BlockRow : ViewModelBase
{
    private LineKind _kind;
    private string _tag = string.Empty;
    private string _text = string.Empty;
    private bool _sealed;
    private bool _expanded;
    private HeadRow? _head;

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

    // Contract: crates/client/src/lib.rs LineKind::folds.
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
    private string _title = string.Empty;
    private bool _sticky;

    public PaneRow(string name, ObservableCollection<BlockRow> blocks)
    {
        Name = name;
        Blocks = blocks;
    }

    public string Name { get; }
    public ObservableCollection<BlockRow> Blocks { get; }

    public string Title
    {
        get => _title;
        set => Set(ref _title, value, nameof(Title));
    }

    public bool Sticky
    {
        get => _sticky;
        set => Set(ref _sticky, value, nameof(Sticky));
    }
}

public sealed class SessionRow : ViewModelBase
{
    private string _status = string.Empty;
    private string _title = string.Empty;
    private bool _unseen;

    public SessionRow(string name)
    {
        Name = name;
    }

    public string Name { get; }

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
        set => Set(ref _title, value, nameof(Title));
    }

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

    public string Mark => _unseen ? "\u25cb" : Status switch
    {
        "running" => "\u25cf",
        "error" => "\u2715",
        _ => "\u25cb",
    };

    public LineKind MarkKind => _unseen
        ? LineKind.ToolCall
        : Status switch
        {
            "running" => LineKind.Result,
            "error" => LineKind.Error,
            _ => LineKind.System,
        };
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
