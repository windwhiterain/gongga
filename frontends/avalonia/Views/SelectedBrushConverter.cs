using System.Globalization;
using Avalonia.Data;
using Avalonia.Data.Converters;
using Avalonia.Media;

namespace Gongga.Avalonia.Views;

/// The candidate a Tab would take: the row has to look different, or the highlight
/// is only in the model and the person cannot see what Tab will insert.
public sealed class SelectedBrushConverter : IValueConverter
{
    public object? Convert(object? value, Type targetType, object? parameter, CultureInfo culture) =>
        new SolidColorBrush(value is true ? Color.Parse("#1E2A35") : Colors.Transparent);

    public object? ConvertBack(object? value, Type targetType, object? parameter, CultureInfo culture) =>
        BindingOperations.DoNothing;
}
