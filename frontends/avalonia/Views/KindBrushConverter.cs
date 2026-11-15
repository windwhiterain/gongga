using System.Globalization;
using Avalonia;
using Avalonia.Data;
using Avalonia.Data.Converters;
using Avalonia.Media;
using Gongga.Avalonia.Wire;

namespace Gongga.Avalonia.Views;

/// Maps the wire's semantic kind to one look. Contract: crates/tui/src/render.rs
/// style_for decides which kinds differ, never the client.
///
/// # Invariant
///
/// The brush comes from the application's dictionary (`App.axaml`), so a kind has one
/// ink and a screen cannot pick a second one for it.
public sealed class KindBrushConverter : IValueConverter
{
    public object? Convert(object? value, Type targetType, object? parameter, CultureInfo culture)
    {
        var kind = value is LineKind lineKind ? lineKind : LineKind.System;
        var name = kind switch
        {
            LineKind.Command => "KindCommandBrush",
            LineKind.User => "KindUserBrush",
            LineKind.Inject => "KindInjectBrush",
            LineKind.Assistant => "KindAssistantBrush",
            LineKind.Reasoning => "KindReasoningBrush",
            LineKind.ToolCall => "KindToolCallBrush",
            LineKind.ToolOutput => "KindToolOutputBrush",
            LineKind.ToolError => "KindToolErrorBrush",
            LineKind.Result => "KindResultBrush",
            LineKind.Error => "KindErrorBrush",
            _ => "KindSystemBrush",
        };
        return Lookup(name);
    }

    /// A named brush of the application's dictionary, or the muted ink when a
    /// designer reads the file without the application around it.
    internal static IBrush Lookup(string name) =>
        Application.Current?.Resources.TryGetResource(name, null, out var found) == true
            && found is IBrush brush
            ? brush
            : Brushes.Gray;

    public object? ConvertBack(object? value, Type targetType, object? parameter, CultureInfo culture) =>
        BindingOperations.DoNothing;
}
