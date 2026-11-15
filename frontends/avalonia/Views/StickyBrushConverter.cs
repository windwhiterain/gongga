using System.Globalization;
using Avalonia.Data;
using Avalonia.Data.Converters;
using Avalonia.Media;

namespace Gongga.Avalonia.Views;

/// The sticky pane is the one framed pane, as in crates/tui/src/render.rs: the
/// accent line is the whole of it.
public sealed class StickyBrushConverter : IValueConverter
{
    public object? Convert(object? value, Type targetType, object? parameter, CultureInfo culture) =>
        KindBrushConverter.Lookup(value is true ? "AccentBrush" : "HairlineBrush");

    public object? ConvertBack(object? value, Type targetType, object? parameter, CultureInfo culture) =>
        BindingOperations.DoNothing;
}
