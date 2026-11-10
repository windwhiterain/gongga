using System.Text.Json;

namespace Gongga.Avalonia.Wire;

public enum LineKind
{
    System,
    Inject,
    Command,
    User,
    Assistant,
    Reasoning,
    ToolCall,
    ToolOutput,
    ToolError,
    Result,
    Error,
}

public enum HeadKind
{
    Call,
    Ok,
    Failed,
    Reasoning,
    Output,
    Cmd,
    Error,
}

public sealed record ListedRow(string Name, string Status, string Title);

public sealed record QuotaRow(string Text, LineKind Kind);

public sealed record WindowLayout(
    IReadOnlyList<ListedRow> Listed,
    IReadOnlyList<string> Shown,
    string Command,
    QuotaRow? Quota);

public sealed record CandidateRow(string Insert, string Detail, bool Space);

public sealed record HeadField(string Label, string Value, bool Summary);

public sealed record HeadRow(
    HeadKind Kind,
    string Name,
    string Description,
    IReadOnlyList<HeadField> Fields)
{
    private string Title => Description.Length == 0 ? Name : $"{Name}\u00b7{Description}";

    private static string Spelled(HeadField item) =>
        item.Label.Length == 0 ? item.Value : $"{item.Label}={item.Value}";

    public string Summary
    {
        get
        {
            var parts = new List<string>();
            if (Name.Length != 0)
            {
                parts.Add(Title);
            }
            foreach (var item in Fields)
            {
                if (item.Summary)
                {
                    parts.Add(Spelled(item));
                }
            }
            return string.Join(", ", parts);
        }
    }

    public string Detail
    {
        get
        {
            var rows = new List<string>();
            if (Name.Length != 0)
            {
                rows.Add(Title);
            }
            foreach (var item in Fields)
            {
                rows.Add(Spelled(item));
            }
            return string.Join("\n", rows);
        }
    }

    // Contract: crates/client/src/lib.rs head_label leaves one trailing space.
    public string Label => Summary.Length == 0 ? string.Empty : $"{Summary} ";

    // Contract: crates/client/src/lib.rs kind_of maps a server-named head to a style.
    public LineKind LineKind => Kind switch
    {
        HeadKind.Call => LineKind.ToolCall,
        HeadKind.Ok => LineKind.ToolOutput,
        HeadKind.Failed => LineKind.ToolError,
        HeadKind.Reasoning => LineKind.Reasoning,
        HeadKind.Output => LineKind.Assistant,
        HeadKind.Cmd => LineKind.Command,
        HeadKind.Error => LineKind.Error,
        _ => LineKind.System,
    };
}

/// Client -&gt; front-end. Contract: crates/client/src/render_wire.rs RenderMessage.
public abstract record RenderFrame
{
    public sealed record Line(string Window, LineKind Kind, string Text) : RenderFrame;

    public sealed record Stream(
        string Window,
        string Key,
        LineKind Kind,
        string Prefix,
        string Text,
        bool Done) : RenderFrame;

    public sealed record Status(string Text) : RenderFrame;

    public sealed record Note(LineKind Kind, string Text) : RenderFrame;

    public sealed record Layout(WindowLayout Value) : RenderFrame;

    public sealed record Rewrite(string Window, string Key, string Text) : RenderFrame;

    public sealed record ToolCall(string Window, string Key, HeadRow Head, bool Streamed) : RenderFrame;

    public sealed record Relabel(string Window, string Key, string Tag, LineKind Kind) : RenderFrame;

    public sealed record Forget(string Window) : RenderFrame;

    public sealed record Clear(string Window) : RenderFrame;

    public sealed record StickySession(string? Name) : RenderFrame;

    public sealed record Stopped(string Session, bool Failed) : RenderFrame;

    public sealed record Draft(string Text) : RenderFrame;

    public sealed record Server(string? Label, bool Ours) : RenderFrame;

    public sealed record Repoint(string Endpoint) : RenderFrame;

    public sealed record Vocabulary(long Version) : RenderFrame;

    public sealed record Completion(
        long Start,
        long End,
        string? Subject,
        IReadOnlyList<CandidateRow> Candidates) : RenderFrame;
}

public static class WireReader
{
    /// An unreadable or unknown frame is skipped, never thrown: one bad line must
    /// not end the window. Contract: crates/tui/src/pipe.rs stdout reader.
    public static RenderFrame? Parse(string line)
    {
        try
        {
            using var document = JsonDocument.Parse(line);
            var root = document.RootElement;
            if (!root.TryGetProperty("type", out var tag))
            {
                return null;
            }
            return tag.GetString() switch
            {
                "line" => new RenderFrame.Line(Text(root, "window"), Kind(root, "kind"), Text(root, "text")),
                "stream" => new RenderFrame.Stream(
                    Text(root, "window"),
                    Text(root, "key"),
                    Kind(root, "kind"),
                    Text(root, "prefix"),
                    Text(root, "text"),
                    Flag(root, "done")),
                "status" => new RenderFrame.Status(Text(root, "text")),
                "note" => new RenderFrame.Note(Kind(root, "kind"), Text(root, "text")),
                "layout" => new RenderFrame.Layout(ReadLayout(root.GetProperty("layout"))),
                "rewrite" => new RenderFrame.Rewrite(Text(root, "window"), Text(root, "key"), Text(root, "text")),
                "tool_call" => new RenderFrame.ToolCall(
                    Text(root, "window"),
                    Text(root, "key"),
                    ReadHead(root.GetProperty("head")),
                    Flag(root, "streamed")),
                "relabel" => new RenderFrame.Relabel(
                    Text(root, "window"),
                    Text(root, "key"),
                    Text(root, "tag"),
                    Kind(root, "kind")),
                "forget" => new RenderFrame.Forget(Text(root, "window")),
                "clear" => new RenderFrame.Clear(Text(root, "window")),
                "sticky_session" => new RenderFrame.StickySession(OptionalText(root, "name")),
                "stopped" => new RenderFrame.Stopped(Text(root, "session"), Flag(root, "failed")),
                "draft" => new RenderFrame.Draft(Text(root, "text")),
                "server" => new RenderFrame.Server(OptionalText(root, "label"), Flag(root, "ours")),
                "repoint" => new RenderFrame.Repoint(Text(root, "endpoint")),
                "vocabulary" => new RenderFrame.Vocabulary(Number(root, "version")),
                "completion" => new RenderFrame.Completion(
                    Number(root, "start"),
                    Number(root, "end"),
                    OptionalText(root, "subject"),
                    ReadCandidates(root)),
                _ => null,
            };
        }
        catch (JsonException)
        {
            return null;
        }
        catch (KeyNotFoundException)
        {
            return null;
        }
        catch (InvalidOperationException)
        {
            return null;
        }
    }

    private static WindowLayout ReadLayout(JsonElement element)
    {
        var listed = new List<ListedRow>();
        if (element.TryGetProperty("listed", out var rows) && rows.ValueKind == JsonValueKind.Array)
        {
            foreach (var row in rows.EnumerateArray())
            {
                listed.Add(new ListedRow(Text(row, "name"), Text(row, "status"), Text(row, "title")));
            }
        }
        var shown = new List<string>();
        if (element.TryGetProperty("shown", out var names) && names.ValueKind == JsonValueKind.Array)
        {
            foreach (var name in names.EnumerateArray())
            {
                if (name.ValueKind == JsonValueKind.String)
                {
                    shown.Add(name.GetString()!);
                }
            }
        }
        QuotaRow? quota = null;
        if (element.TryGetProperty("quota", out var value) && value.ValueKind == JsonValueKind.Object)
        {
            quota = new QuotaRow(Text(value, "text"), Kind(value, "kind"));
        }
        return new WindowLayout(listed, shown, Text(element, "command"), quota);
    }

    private static HeadRow ReadHead(JsonElement element)
    {
        var fields = new List<HeadField>();
        if (element.TryGetProperty("fields", out var values) && values.ValueKind == JsonValueKind.Array)
        {
            foreach (var field in values.EnumerateArray())
            {
                fields.Add(new HeadField(
                    Text(field, "label"),
                    Text(field, "value"),
                    Flag(field, "summary")));
            }
        }
        return new HeadRow(
            OptionalText(element, "kind") switch
            {
                "ok" => HeadKind.Ok,
                "failed" => HeadKind.Failed,
                "reasoning" => HeadKind.Reasoning,
                "output" => HeadKind.Output,
                "cmd" => HeadKind.Cmd,
                "error" => HeadKind.Error,
                _ => HeadKind.Call,
            },
            Text(element, "name"),
            Text(element, "description"),
            fields);
    }

    private static IReadOnlyList<CandidateRow> ReadCandidates(JsonElement root)
    {
        var candidates = new List<CandidateRow>();
        if (root.TryGetProperty("candidates", out var values) && values.ValueKind == JsonValueKind.Array)
        {
            foreach (var candidate in values.EnumerateArray())
            {
                candidates.Add(new CandidateRow(
                    Text(candidate, "insert"),
                    Text(candidate, "detail"),
                    Flag(candidate, "space")));
            }
        }
        return candidates;
    }

    private static string Text(JsonElement element, string name) =>
        element.ValueKind == JsonValueKind.Object
        && element.TryGetProperty(name, out var value)
        && value.ValueKind == JsonValueKind.String
            ? value.GetString()!
            : string.Empty;

    private static string? OptionalText(JsonElement element, string name) =>
        element.ValueKind == JsonValueKind.Object
        && element.TryGetProperty(name, out var value)
        && value.ValueKind == JsonValueKind.String
            ? value.GetString()
            : null;

    private static bool Flag(JsonElement element, string name) =>
        element.ValueKind == JsonValueKind.Object
        && element.TryGetProperty(name, out var value)
        && value.ValueKind == JsonValueKind.True;

    private static long Number(JsonElement element, string name) =>
        element.ValueKind == JsonValueKind.Object
        && element.TryGetProperty(name, out var value)
        && value.TryGetInt64(out var number)
            ? number
            : 0;

    private static LineKind Kind(JsonElement element, string name) => Text(element, name) switch
    {
        "inject" => LineKind.Inject,
        "command" => LineKind.Command,
        "user" => LineKind.User,
        "assistant" => LineKind.Assistant,
        "reasoning" => LineKind.Reasoning,
        "tool_call" => LineKind.ToolCall,
        "tool_output" => LineKind.ToolOutput,
        "tool_error" => LineKind.ToolError,
        "result" => LineKind.Result,
        "error" => LineKind.Error,
        _ => LineKind.System,
    };
}
