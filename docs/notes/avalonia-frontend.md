# frontends/avalonia — the second front-end

## Where it sits and why

`frontends/avalonia/` is a standalone .NET project. It is **not** a Cargo
workspace member and is **not** in the bundle. Avalonia is a .NET/C# UI
framework, so a `crates/avalonia` would imply a Rust crate that does not exist;
`DESIGN.md` §1 verdict 2 says only the TUI ships in the bundle and other UIs
distribute themselves.

`gongga-client.exe` is resolved without an argument, in this order: `--client
<path>`, `GONGGA_CLIENT_EXE`, beside the window's own assembly (what a bundle
extracts), then walking up from `bin/Debug/net10.0` to the repository's
`target/debug` and `target/release`. `--client` exists only for a build the
search cannot see.

## Running it

The SDK lives under `%LOCALAPPDATA%\Microsoft\dotnet`, and that directory is on
the user `PATH`, so one line from this directory starts the window:

    dotnet run

The headless check needs only the transport flags:

    dotnet run -- --no-window --server 127.0.0.1:55901 --client_only \
        --home <throwaway> --client-home <throwaway> --key <any>

`--server` matters for a check: without it the client dials the default
`127.0.0.1:9700`, which may be a server somebody is actually using.

Nothing on the Rust side had to change. `DESIGN.md` §1 already decided that a
front-end is cross-process and cross-language, and `crates/client/src/main.rs`
already speaks one `UiIntent` per stdin line and one `RenderMessage` per stdout
line. `crates/tui/src/pipe.rs` is the reference host; this front-end is its C#
counterpart.

## The contract

Source of truth: `crates/client/src/render_wire.rs`.

| direction | type | tag | count |
|---|---|---|---|
| client -> UI | `RenderMessage` | `type`, internal, snake_case | 17 variants: 11 drawing, 6 control |
| UI -> client | `UiIntent` | `intent`, internal, snake_case | 5 variants |
| client -> itself | `CursorMessage` | `type` | never on the pipe |

`docs/notes/client-render-wire.md` still says "12 drawing + 5 control"; the code
has 11 drawing + 6 control (`StickySession` is a control frame). The C# parser
follows the code. `Option<T>` fields arrive as `null`, never omitted.

`RenderMessage` is semantic: `LineKind` says what a row is, and the front-end
owns colour, wrapping and columns. `Wire/WireTypes.cs` reads frames with
`JsonDocument` and skips an unreadable or unknown frame instead of throwing; a
bad line must never close the window.

## Frame handling

`ViewModels/ClientModel.cs` is the state machine. All frames arrive on the
Avalonia UI thread through `Dispatcher.UIThread.Post`, so `ObservableCollection`
is safe and a frame is applied in the order it was written.

- `Line` starts a sealed block. `Stream` accumulates by **key**, not by window;
  `prefix` is a label and is excluded from the body; `done` seals the key.
- `Rewrite` replaces a whole body by key, `Relabel` replaces tag **and** kind by
  key and can change the colour retroactively, `ToolCall` creates or replaces a
  block with a structured `Head`.
- `Clear` empties a window's blocks but keeps the pane; `Forget` removes the pane
  and marks its blocks dropped; `Repoint` forgets every window.
- `Status` and `Note` share the one status line, and `Status` forces the kind back
  to `System`.
- `Vocabulary` is a request, not a drawing: the front-end answers with a
  `Complete` intent from its current input. `Completion` then arrives separately.
- `Draft`, `Server`, `Repoint`, `StickySession` and `Stopped` update view state
  (`Stopped` also marks an unshown session and shows a toast).

`Layout` rebuilds the session list and the shown panes **incrementally** so a
pane keeps its `ScrollViewer` and scroll offset. `Layout.listed` is newest-turn
first and `Layout.shown` is oldest first; the two orders are deliberately
different.

## Cursors are bytes

`UiIntent::Complete.cursor` and `Completion.start/end` are UTF-8 **byte**
offsets on a character boundary, while an Avalonia `TextBox.CaretIndex` is a
UTF-16 index. `Wire/Utf8Cursor.cs` converts both ways, clamps to a rune
boundary, and applies a candidate over the byte range. Getting this wrong
corrupts any non-ASCII input.

## Gestures

| gesture | intent |
|---|---|
| Enter | `Submit` |
| Shift+Enter / Alt+Enter | newline (no intent) |
| input change / Tab | `Complete`, then `AcceptCompletion` |
| Esc / Up / Down in popup | close / move selection |
| Ctrl+Q | `Quit` |
| Shift+Delete with empty input | `Operate(DeleteSession)` on the sticky session |
| plain click on a session | `Operate(RenderSession { show: true, solo: true })` |
| Shift/Ctrl+click on a session | `Operate(RenderSession { solo: false, show: !shown })` |
| click on a pane header | `Operate(StickySession)` |
| PageUp / PageDown | scroll the sticky pane |
| wheel a shown pane to its oldest row | `Older { windows }` |
| click a folding block | expand / collapse its abstract |

`render_wire.rs` comments `UiIntent::Quit` as "Alt+Q", and `docs/notes/tui-run.md`
repeats it, but the TUI implementation uses **Ctrl+Q** (`crates/tui/src/lib.rs`)
and swallows all Alt keys. The GUI binds Ctrl+Q.

## Process lifecycle

`Wire/ClientProcess.cs` spawns exactly one child with `--parent <our pid>`
plus the connection flags, pipes stdin/stdout, and continuously drains stderr
into `<client-home>/client.log`. Closing stdin is the window leaving; an ended
stdout closes the window. It never kills by image name — only the `Process`
handle it owns.

`--no-window` runs the same transport and frame model without creating an
Avalonia window, prints one verdict line (`PASS/FAIL · frames · types · ...`)
and returns an exit code. That is the machine-readable L3 check for a real
`gongga-client` child on a headless run.

## Deliberate limits

- `LineKind::Assistant` is shown as wrapped plain text, not rendered markdown.
  The wire carries text, not a document, and a GUI markdown engine would be a
  second renderer with its own layout rules.
- `block_size` and `box_edge` are not on the wire (`client-render-wire.md`
  "Not done yet"). The GUI keeps every block in memory and folds by kind only.
- The completion popup uses keyboard selection and a click to accept; it does
  not mirror the terminal's popup geometry.

## Toolchain

Avalonia 12's XAML source generators require Roslyn 4.14, which ships with the
.NET 10 SDK. A .NET 8 SDK compiles the C# but cannot run the generators:
`InitializeComponent` is missing and `CS9057` is reported even for the official
`dotnet new avalonia.app` template. The project targets `net10.0`.
