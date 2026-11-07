# client/render_wire.rs

## Why a wire vocabulary at all

`Sink` has always been a protocol: its 17 methods carry a window name, a closed
set of semantic kinds, and text -- never a ratatui type. It was only expressed as
a Rust trait, so a front-end had to be Rust and had to live in the same process.
With `[client]` now a binary (SPEC `## Crate Structure`, 2026-11-07), the client
is a daemon and the front-end can be any language. `render_wire.rs` writes the
same vocabulary down as serializable values; it does not invent a second protocol.

See `.agents/notes/2026-11-07-client-library-and-rpc-layer.md` for the verdict.

## The three enums

| enum | direction | variants |
| --- | --- | --- |
| `RenderMessage` | daemon -> front-end | the 12 drawing methods of `Sink` |
| `CursorMessage` | daemon -> itself | the 5 journal methods of `Sink` |
| `UiIntent` | front-end -> daemon | `Submit`, `Operate`, `Older`, `Quit` |

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

`UiIntent` mirrors `tui::UiEvent` minus `Recomplete`, which is a terminal-only
signal that the completion wants the loop to go round. Two enums currently spell
the same four gestures; unifying them by lifting `UiEvent` into `client` is a
later step, registered as debt here rather than hidden.

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

## Not done yet

`WireSink` is not connected to any runtime. Missing, in order: a binary target for
`crates/client`, a transport (WebSocket or stdio), a `Runtime` that owns one
`Client` and broadcasts render messages to every connected front-end, a snapshot
for a new connection (`get_messages --replay true`, with `last_seq` and `as_of`),
the window journal's move into the daemon (`log.rs`, `cache.rs`, `panes.rs`), a
TUI reduced to a protocol client, and bundle routing. The ownership invariants
these steps touch are listed as **【待确认】** in `DESIGN.md` §1.
