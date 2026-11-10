using System.Globalization;
using Avalonia.Data;
using Avalonia.Data.Converters;
using Avalonia.Media;

namespace Gongga.Avalonia.Views;

/// The sticky pane is the one framed pane, as in crates/tui/src/render.rs.
public sealed class StickyBrushConverter : IValueConverter
{
    public object? Convert(object? value, Type targetType, object? parameter, CultureInfo culture) =>
        new SolidColorBrush(value is true ? Color.Parse("#3E8CA0") : Color.Parse("#2A3038"));

    public object? ConvertBack(object? value, Type targetType, object? parameter, CultureInfo culture) =>
        BindingOperations.DoNothing;
}
