using System.Globalization;
using Avalonia.Data;
using Avalonia.Data.Converters;
using Avalonia.Media;
using Gongga.Avalonia.Wire;

namespace Gongga.Avalonia.Views;

/// Maps the wire's semantic kind to one look. Contract: crates/tui/src/render.rs
/// style_for decides colour, never the client.
public sealed class KindBrushConverter : IValueConverter
{
    public object? Convert(object? value, Type targetType, object? parameter, CultureInfo culture)
    {
        var kind = value is LineKind lineKind ? lineKind : LineKind.System;
        if (parameter as string == "header")
        {
            return new SolidColorBrush(kind switch
            {
                LineKind.Error => Color.Parse("#8B2020"),
                LineKind.Command or LineKind.Result => Color.Parse("#205B34"),
                _ => Color.Parse("#175C6B"),
            });
        }
        return new SolidColorBrush(kind switch
        {
            LineKind.System or LineKind.Reasoning => Color.Parse("#8A8A8A"),
            LineKind.Command => Color.Parse("#C060C0"),
            LineKind.User => Color.Parse("#E89AE8"),
            LineKind.Inject => Color.Parse("#78C8C8"),
            LineKind.Assistant => Color.Parse("#ECECEC"),
            LineKind.ToolCall => Color.Parse("#D8B84C"),
            LineKind.ToolOutput => Color.Parse("#78A8DC"),
            LineKind.ToolError => Color.Parse("#E08080"),
            LineKind.Result => Color.Parse("#78C878"),
            LineKind.Error => Color.Parse("#F06060"),
            _ => Color.Parse("#8A8A8A"),
        });
    }

    public object? ConvertBack(object? value, Type targetType, object? parameter, CultureInfo culture) =>
        BindingOperations.DoNothing;
}
