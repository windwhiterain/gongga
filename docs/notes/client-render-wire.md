# client/render_wire.rs

## Why a wire vocabulary at all

`Sink` has always been a protocol: its methods carry a window name, a closed set
of semantic kinds, and text -- never a ratatui type. It was only expressed as
a Rust trait, so a front-end had to be Rust and had to live in the same process.
With `[client]` now a binary (SPEC `## Crate Structure`, 2026-11-07), the UI
spawns it as a child process and the front-end can be any language. `render_wire.rs` writes the
same vocabulary down as serializable values; it does not invent a second protocol.

See `.agents/notes/2026-11-07-client-library-and-rpc-layer.md` for the verdict.

## The three enums

| enum | direction | variants |
| --- | --- | --- |
| `RenderMessage` | client -> front-end | every variant in `crates/client/src/render_wire.rs`: drawings become `Sink` calls, control frames land on the window's state |
| `CursorMessage` | client -> itself | `Settled`, `Transcript`, `Rewind`, `Older`, `Landed` -- the journal's answers, never on the pipe |
| `UiIntent` | front-end -> client | `Submit`, `Operate`, `Older`, `Newer`, `Held`, `Room`, `Columns`, `Complete`, `Quit` |

`RenderMessage` is semantic, not pixel-level: `LineKind` says *what* a row is and
the front-end decides its colour, column width and wrapping. Putting a colour on
the wire is explicitly rejected in `docs/notes/protocol-message.md` (`a color:
"green" on the wire`), and two front-ends disagreeing about layout would otherwise
need a negotiation on the live connection.

`CursorMessage` is not for a front-end. `transcript`, `rewind`, `older`, `landed`
and `settled` are the answers behind `Client::declare_drawn` and
`ask_transcripts`; they are how the client and the window journal agree on what
was drawn. A stateless front-end neither holds a journal nor answers those
questions, so the daemon consumes these itself. Encoding them in Rust keeps one
place where every `Sink` method has a name, without putting them on the wire.

`UiIntent` is the only definition of the gestures. `tui::UiEvent` is a wrapper
over it: `Intent(UiIntent)` or `Recomplete`. `Recomplete` is a terminal-only
signal that the completion wants the loop to go round; it is not an intent and
never crosses the wire.

## The control frames

A `Sink` can only say "draw this". Some things a window does are not drawings, so
they are `RenderMessage` variants rather than a second protocol:

| frame | sent by | what the UI does |
| --- | --- | --- |
| `Draft { text }` | `restore_input` | put the unsent line back in the input |
| `Server { label, ours }` | `set_server` | draw the header's own / not own label |
| `Repoint { endpoint }` | `repoint` | drop the old endpoint's window rows |
| `Vocabulary { version }` | `refresh_completion` | re-ask for the current line's words |
| `Completion { line, cursor, start, end, subject, items }` | `show_completion` | open the popup, if it is still about this line |
| `StickySession { name }` | `sticky_session` | move the pane a key or a click acts on |
| `Row { window, seq }` | `row` | state the stored row the frames after it draw |
| `Older { window, rows }` / `Newer { window, rows }` | `prepend` / `append_page` | draw a finished page above / below what a window holds |

`Render { block_size, box_edge, keep_rows }` is the window's own settings, `Stopped
{ session, failed }` is its news, and `Rewind { window }` drops the blocks a window
had not finished. The drawings proper are `Line`, `Stream`, `Document`, `Rewrite`,
`ToolCall`, `Relabel`, `Forget`, `Clear`, `Status`, `Note` and `Layout`, plus
`Stopped` through the sink.

`UiIntent::Complete { line, cursor }` is the request behind the last two. The
loop clamps a byte cursor to a character boundary, asks the client's own
`SharedCompleter`, and hands a `Completion` to `Frontend::show_completion`: the
catalog never crosses to the UI, so command syntax stays in the client. The pipe
front-end encodes that answer as the `Completion` frame; the window keeps no
vocabulary of its own, so Tab applies what the client last sent. The frame names
the line and caret it answers, because the window keeps reading keys while the
round trip is out and an answer about other words must not edit this line.
`Vocabulary` carries the client's real `paths_version`, so a UI can dedupe
refreshes. `Item` gained serde derives for this, so a completion's candidates are
the same struct the client applies.

`UiIntent::Room { windows }` is the one fact that travels the other way. Only the
drawing knows how many rows a pane has, and one page of the past is sized by that
number, so the UI reports each pane's height when it changes. It is not per frame:
an unchanged height is a page already sized.

## Why `WireSink` overrides `tool_call`

The trait's default implementation of `tool_call` downgrades a structured
`protocol::Head` into `rewrite` plus `relabel` text. `Head` already derives
`Serialize`/`Deserialize`, so `WireSink` keeps the structure intact and a browser
can show a call's fields instead of a flattened sentence. Every other method is
encoded verbatim.

`Listed`, `Layout` and `QuotaRow` gained serde derives for the same reason. They
are plain data; no drawing code moved.

## One definition, not three

The Rust types with serde derives are the only definition. A front-end in another
language reads them by their `type` tag; hand-writing a parallel schema would
recreate the duplication `crates/client` exists to prevent.

## Frames on the pipe

`encode_frame` writes one `RenderMessage` as one JSON line, with the newline
outside the value; `decode_intent` reads one `UiIntent` from a line the UI wrote.
`serde_json` escapes newlines inside strings, so `Submit { line }` holding
paragraphs is still a single frame. Both return a `Result` rather than panicking
on a malformed value.

These are pure functions on purpose. The writer is **not** a `Sink`: `Sink` is
synchronous `&mut`, and a blocking `write_all` to a pipe whose reader is slow
would stall the client's state machine. The runtime sends encoded frames to a
writer task over an unbounded queue, and the UI drains stdout continuously.
Dropping a frame when a pipe is full would lose a line the operator waits for.
`WireSink` collects the messages; this module only spells them.

## Not done yet

`gongga-client` is a real binary, `runtime::run` owns the loop and one `Client`,
and `WindowJournal` / `Roster` hold the window's records in the client library.
The TUI now spawns the binary by default and the bundle carries it (steps 6 and
7), so `encode_intent` is the UI's half of the round trip. Steps 8 (multi-client
L3) and the in-process fallback deletion are done. A-4 remains, but its original
shape -- splitting `Entry`'s data blocks into the client -- is stale after the
fallback went away; see `DESIGN.md` §1, "A-4 复核", for the two directions
awaiting a decision.

Two different links, two different transports. UI <-> client is one-to-one and
local: stdin carries `UiIntent`, stdout carries `RenderMessage`, one JSON value
per line, no socket. client <-> server stays SPEC's reliable UDP: many clients
share one server and that server may be remote, so a pipe cannot replace it.
`crates/client/src/server.rs:101` already refuses to spawn a server at a
non-loopback address, and `:258` redirects a spawned server's stdout and stderr
to a log file, keeping the protocol pipe clean.

The pipe has sharp edges. `crates/server/src/remote/transport.rs:334-345`
(`Stdio::piped()` on all three streams, `kill_on_drop`) and `:815-850` (a
line-writing loop and a `lines().next_line()` reader) are the existing wheel for
this shape. stderr must be drained or sent to a log: `NullSink::note` uses
`eprintln!`, and an unread pipe eventually blocks the client's main loop.
`WireSink` is a synchronous `&mut Sink`, so the writer is its own task: the pipe
front-end collects encoded frames and writes them from a task, while the UI side
drains stdout continuously. A bounded queue's two answers -- drop a frame or stall
the loop -- are both worse than the unbounded channel it uses. `DESIGN.md` §1
"形态 2" holds the four hazards; the `--parent` question there was answered (the
UI's pid).

## A row's place travels with it (2026-11-15)

Three frames of this wire say where rows belong in the transcript rather than only
what they say: `RenderMessage::Row { window, seq }` states the stored row the calls
after it draw, `PastRow::Line`/`Block` carry their own `seq` (and a block its `from`,
because a block spans rows), and `RenderMessage::Newer { window, rows }` is a page of
what comes *after* what the window holds -- one frame of finished rows, the mirror of
`Older`. Without the numbers a front-end could only guess at the range it holds, and a
guess is what makes a page land twice or leave a hole.

## A page's body is the same body (2026-11-19)

A block carries `rows` whether it came from a stream or from a page. A streamed answer
arrives as text and is restated as a `Document`; a paged one used to arrive as text and
nothing else, so a front-end drew the same answer two ways: styled while it streamed, and
as plain text once the operator scrolled back -- dim gray with its markdown unread. The
client now fits a page's answers to the pane's width (`PipeFrontend::page_rows`), keeps
them in `answers` so a width change restates them the way it restates a live one, and
`PastRow::Block.rows` is what both front-ends draw. One rule for a body, wherever it came
from: an answer is rows, and every other kind is text.

## ToolCall::started_at — the frame carries the start (2026-11-19)

The frame that describes a call carries the server's `started_at` beside the head, so
the window that draws it needs no clock of its own to begin: the number it counts
from crossed the pipe with the call. `serde(default)` keeps a frame from an older
client readable, reading as `0`, which the window treats as "no start was kept".
