using System.Globalization;
using Avalonia.Data;
using Avalonia.Data.Converters;
using Avalonia.Media;
using Gongga.Avalonia.Wire;

namespace Gongga.Avalonia.Views;

/// The ink a block's own words are written in.
///
/// # Invariant
///
/// SPEC gives the operator's words and the answer the brightest ink and leaves the
/// rest quiet: the mark already says what a row is, so the body does not have to
/// shout it. Contract: crates/tui/src/render.rs content_for.
public sealed class ContentBrushConverter : IValueConverter
{
    public object? Convert(object? value, Type targetType, object? parameter, CultureInfo culture) =>
        KinBrush(value);

    private static IBrush KinBrush(object? value) => value switch
    {
        LineKind.User or LineKind.Assistant => KindBrushConverter.Lookup("InkBrush"),
        LineKind.Reasoning => KindBrushConverter.Lookup("KindReasoningBrush"),
        _ => KindBrushConverter.Lookup("InkSoftBrush"),
    };

    public object? ConvertBack(object? value, Type targetType, object? parameter, CultureInfo culture) =>
        BindingOperations.DoNothing;
}
