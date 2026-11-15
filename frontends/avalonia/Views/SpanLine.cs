using Avalonia;
using Avalonia.Controls;
using Avalonia.Controls.Documents;
using Avalonia.Media;
using Gongga.Avalonia.Wire;

namespace Gongga.Avalonia.Views;

/// One row of a document: the runs the client sent, and nothing else.
///
/// # Invariant
///
/// The runs are built once, when the row is handed its spans: a row is replaced whole
/// when the body changes, so nothing here ever rewrites a collection a text layout is
/// already reading. Every class is drawn from the application's own dictionary, so a
/// run has one look and this control has no colour of its own.
public sealed class SpanLine : TextBlock
{
    public static readonly StyledProperty<IReadOnlyList<SpanItem>?> SpansProperty =
        AvaloniaProperty.Register<SpanLine, IReadOnlyList<SpanItem>?>(nameof(Spans));

    static SpanLine() =>
        SpansProperty.Changed.AddClassHandler<SpanLine>((line, _) => line.Build());

    public IReadOnlyList<SpanItem>? Spans
    {
        get => GetValue(SpansProperty);
        set => SetValue(SpansProperty, value);
    }

    private void Build()
    {
        var runs = new InlineCollection();
        foreach (var span in Spans ?? [])
        {
            runs.Add(Run(span));
        }
        Inlines = runs;
    }

    /// A line through the words, as SPEC's `~~strike~~` says.
    private static readonly TextDecorationCollection Struck =
        new() { new TextDecoration { Location = TextDecorationLocation.Strikethrough } };

    private Run Run(SpanItem span) => span.Class switch
    {
        "strong" => new Run { Text = span.Text, FontWeight = FontWeight.SemiBold },
        "emphasis" => new Run { Text = span.Text, FontStyle = FontStyle.Italic },
        "strike" => new Run { Text = span.Text, TextDecorations = Struck },
        "code" => new Run
        {
            Text = span.Text,
            Background = KindBrushConverter.Lookup("CodeBrush"),
            Foreground = KindBrushConverter.Lookup("InkBrush"),
        },
        "marker" => new Run { Text = span.Text, Foreground = KindBrushConverter.Lookup("InkFaintBrush") },
        "link" => new Run { Text = span.Text, Foreground = KindBrushConverter.Lookup("AccentInkBrush") },
        _ => new Run { Text = span.Text },
    };
}
