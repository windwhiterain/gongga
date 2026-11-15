using System.Globalization;
using Avalonia.Data;
using Avalonia.Data.Converters;
using Avalonia.Media;
using Gongga.Avalonia.Wire;

namespace Gongga.Avalonia.Views;

/// The surface a block's body sits on.
///
/// # Invariant
///
/// An answer and the operator's own words are prose on the pane; a tool's output is a
/// wall of paths and JSON, and it reads as one only when it is set on its own surface.
public sealed class BodyBrushConverter : IValueConverter
{
    public object? Convert(object? value, Type targetType, object? parameter, CultureInfo culture) =>
        value is LineKind.ToolCall or LineKind.ToolOutput or LineKind.ToolError or LineKind.Reasoning
            ? KindBrushConverter.Lookup("CodeBrush")
            : Brushes.Transparent;

    public object? ConvertBack(object? value, Type targetType, object? parameter, CultureInfo culture) =>
        BindingOperations.DoNothing;
}
