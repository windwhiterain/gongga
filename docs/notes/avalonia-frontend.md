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

## A second window

A client claims its home with `client.lock` (see [`home-lock.md`](home-lock.md)),
so two windows pointed at one client home do not both run: the second client
exits with `another gongga client is already using this home`, and the window
keeps itself open showing that line rather than closing silently. Give the
second window its own home:

    dotnet run -- --instance work

`--instance <name>` maps to `<client-home>/<name>`, where the base directory is
`--client-home` when given and `~/.gongga-client` otherwise. The name must be a
plain name, not a path.

`client.toml` -- sticky session, shown sessions, block size, the remembered address --
is in the **client** home too, so each instance keeps its own. A home that predates
that move is copied forward once, from `<harness-home>/client.toml`; see
[`tui-state.md`](tui-state.md).

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
stdout closes the window, unless no `Layout` ever arrived — then the child never
connected, and its last log line is shown instead of a window that disappears.
It never kills by image name — only the `Process` handle it owns.

`--no-window` runs the same transport and frame model without creating an
Avalonia window, prints one verdict line (`PASS/FAIL · frames · types · ...`)
and returns an exit code. That is the machine-readable L3 check for a real
`gongga-client` child on a headless run.

## Looking at it without a screen

`--shots <dir>` builds the real `MainWindow` on Avalonia's headless platform with
Skia, so the window renders at full fidelity into a bitmap and nothing appears on
anyone's desktop:

    dotnet run -- --shots C:/tmp/shots --home <home> --client-home <home> \
        --server 127.0.0.1:<port> --key <key>

It writes `01-before-frames.png` through `06-submitted.png`, plus a `tree*.txt`
dump of every control with the box it got. The run also clicks a listed session,
types `/set_re`, presses Tab, types a whole command and presses Enter, so the
pictures cover the states that matter: empty, connected, pane open, completion,
and a submitted command.

Why not a virtual desktop: Windows desktops are one session with one input queue
and one visible desktop at a time. Switching to another takes the screen away
from the person using the machine, and `SendInput` only reaches the foreground
window of the current desktop, so it never bought the isolation it looked like.
Off-screen rendering is the only way to look at a window without taking the
desktop.

The mode found four bugs on its first run, each fixed here:

- **The caret did not travel with a text this process chose.** A completion, a
  recalled line and a restored draft were written into the model, and the bound
  `TextBox` kept its own caret, so the next keystroke landed in the middle of the
  word (`/set_re --id delta...nder `). `ClientModel.SetInput` now hands text and
  caret to the window, which writes both.
- **Enter never submitted.** An `AcceptsReturn` `TextBox` inserts the newline in
  its own handler, which runs before a bubbling `KeyDown`, so the submit branch
  was unreachable. The handler is attached as a tunnel handler now.
- **The window was bigger than the screen.** 1280x820 DIP at 175% scaling is
  2264x1468 physical; opened at `@264,264` its input row sat at y=1634 on a
  1440-high screen. `OnOpened` caps the size to the working area and centres it.
- **Nothing said why the pane area was empty.** It now says
  `No session open — click one on the left.`

Two more came from the same pictures: an unfocused input box was invisible (the
border was the same colour as the background) and now has a border and a
placeholder, and a long session subtitle was cut mid-word without an ellipsis.

The fifth is the one the pictures did not show me and the operator did: with two
panes open, only one row in the list looked open. The list was a `ListBox`, so
the row it lit was the row the pointer last hit, which is a fact about the mouse
and not about the window. It is an `ItemsControl` now and a row is lit exactly
when `Layout.shown` names it -- the same rule the terminal uses, where a shown
session's name is the bright one. `tree.txt` carries `[shown]`/`[hidden]` on each
row, so the count is a check and not an opinion: none before a click, one after,
two once the second pane opens.

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
