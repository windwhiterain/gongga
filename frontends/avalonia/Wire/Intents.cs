using System.Text;
using System.Text.Json;

namespace Gongga.Avalonia.Wire;

/// Front-end -&gt; client. Contract: crates/client/src/render_wire.rs UiIntent.
public static class Intents
{
    public static string Submit(string line) => Encode(new { intent = "submit", line });

    public static string Quit() => "{\"intent\":\"quit\"}\n";

    public static string Complete(string line, int caret) => Encode(new
    {
        intent = "complete",
        line,
        cursor = Utf8Cursor.ByteOffset(line, caret),
    });

    public static string Older(IReadOnlyCollection<string> windows) => Encode(new
    {
        intent = "older",
        windows = windows.ToArray(),
    });

    public static string StickySession(string session) => Encode(new
    {
        intent = "operate",
        operation = new { op = "sticky_session", session },
    });

    public static string RenderSession(string session, bool show, bool solo) => Encode(new
    {
        intent = "operate",
        operation = new { op = "render_session", session, show, solo },
    });

    public static string DeleteSession(string session) => Encode(new
    {
        intent = "operate",
        operation = new { op = "delete_session", session },
    });

    private static string Encode<T>(T value) => JsonSerializer.Serialize(value) + "\n";
}

/// A caret in Avalonia is a UTF-16 index; the wire's `cursor`, `start` and `end`
/// are UTF-8 byte offsets on a character boundary.
/// Contract: crates/client/src/runtime.rs clamps a byte cursor to a char boundary.
public static class Utf8Cursor
{
    public static int ByteOffset(string text, int utf16Index)
    {
        var index = Math.Clamp(utf16Index, 0, text.Length);
        if (index < text.Length && char.IsLowSurrogate(text[index]))
        {
            index--;
        }
        return Encoding.UTF8.GetByteCount(text.AsSpan(0, index));
    }

    public static int CharOffset(string text, long byteOffset)
    {
        var bytes = Encoding.UTF8.GetBytes(text);
        var target = (int)Math.Clamp(byteOffset, 0, bytes.Length);
        while (target > 0 && target < bytes.Length && (bytes[target] & 0xc0) == 0x80)
        {
            target--;
        }
        return Encoding.UTF8.GetCharCount(bytes, 0, target);
    }

    /// Replaces the wire's byte range [start, end) with one candidate insertion.
    /// Contract: crates/client/src/complete.rs apply_completion.
    public static (string Text, int Caret) ApplyCompletion(
        string text,
        long start,
        long end,
        CandidateRow candidate)
    {
        var from = CharOffset(text, start);
        var to = CharOffset(text, end);
        if (to < from)
        {
            (from, to) = (to, from);
        }
        var insertion = candidate.Insert;
        var head = text[..from];
        var tail = text[to..];
        if (candidate.Space && (tail.Length == 0 || tail[0] != ' '))
        {
            insertion += " ";
        }
        return (head + insertion + tail, (head + insertion).Length);
    }
}
