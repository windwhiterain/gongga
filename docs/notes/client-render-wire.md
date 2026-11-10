# client/render_wire.rs

## Why a wire vocabulary at all

`Sink` has always been a protocol: its 17 methods carry a window name, a closed
set of semantic kinds, and text -- never a ratatui type. It was only expressed as
a Rust trait, so a front-end had to be Rust and had to live in the same process.
With `[client]` now a binary (SPEC `## Crate Structure`, 2026-11-07), the UI
spawns it as a child process and the front-end can be any language. `render_wire.rs` writes the
same vocabulary down as serializable values; it does not invent a second protocol.

See `.agents/notes/2026-11-07-client-library-and-rpc-layer.md` for the verdict.

## The three enums

| enum | direction | variants |
| --- | --- | --- |
| `RenderMessage` | client -> front-end | the 12 drawing methods of `Sink`, plus 5 control frames |
| `CursorMessage` | client -> itself | the 5 journal methods of `Sink` |
| `UiIntent` | front-end -> client | `Submit`, `Operate`, `Older`, `Complete`, `Quit` |

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

## The five control frames

A `Sink` can only say "draw this". Four things the window does are not drawings,
so they are `RenderMessage` variants rather than a second protocol:

| frame | sent by | what the UI does |
| --- | --- | --- |
| `Draft { text }` | `restore_input` | put the unsent line back in the input |
| `Server { label, ours }` | `set_server` | draw the header's own / not own label |
| `Repoint { endpoint }` | `repoint` | drop the old endpoint's window rows |
| `Vocabulary { version }` | `refresh_completion` | re-ask for the current line's words |
| `Completion { start, end, subject, candidates }` | `show_completion` | open the popup |

`UiIntent::Complete { line, cursor }` is the request behind the last two. The
loop clamps a byte cursor to a character boundary, asks the client's own
`SharedCompleter`, and hands a `Completion` to `Frontend::show_completion`: the
catalog never crosses to the UI, so command syntax stays in the client. The TUI
computes its own popup locally and ignores that callback; the pipe front-end
encodes it. `Vocabulary` carries the client's real `paths_version`, so a UI can
dedupe refreshes. `Candidate` gained serde derives for this.

## Why `WireSink` overrides `tool_call`

The trait's default implementation of `tool_call` downgrades a structured
`protocol::Head` into `rewrite` plus `relabel` text. `Head` already derives
`Serialize`/`Deserialize`, so `WireSink` keeps the structure intact and a browser
can show a call's fields instead of a flattened sentence. Every other method is
encoded verbatim.

`Listed`, `Layout` and `QuotaRow` gained serde derives for the same reason. They
are plain data; no drawing code moved.

## One definition, not three

The Rust types with serde derives are the only definition. TypeScript and Python
types are generated from them; hand-writing a parallel schema would recreate the
duplication `crates/client` exists to prevent.

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
`WireSink` is a synchronous `&mut Sink`, so the writer has to be its own task
with a bounded queue and `try_send`; the UI side must drain stdout continuously.
See `DESIGN.md` §1 for the four hazards and the `--parent` question still open.
