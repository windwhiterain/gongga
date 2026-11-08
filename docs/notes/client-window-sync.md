# client/window_sync.rs

## window_sync.rs

Two things happen after a keystroke or a burst of wire bytes: the window's own
state has to reach the view and the completion, and a command the server
accepted has to write its sticky entries back. Both used to live in
`crates/tui/src/run.rs`. As of 2026-11-08 they live here, unchanged in order and
meaning.

`sync` is one place setting six things, in this order: the state-file settings
(`show_state`), the client's sticky vocabulary, the shown sessions, the
completion vocabulary built by `Window::refresh`, the front-end's completion
refresh, and finally the lines a `Client::refresh` produces. A front-end that
set half of them would draw one state and complete against another.

`settle` walks `take_settled()` and only writes back entries for an *accepted*
command: a refusal leaves `client.toml` as it was, which is why nothing was
written when the line was sent. `command::follows` then forgets what follows a
changed model unless the command stated it too (SPEC's `sticky to xxx`).

## WindowView

`sync` and `settle` take `&mut dyn WindowView` instead of `&mut Tui`. Four
methods, all local, none serialized:

| method | what it does |
| --- | --- |
| `show_state(&State)` | the block size, the box edge, the sticky session |
| `refresh_completion()` | the completion's own vocabulary changed |
| `refresh_lines(&Client)` | draw the refresh with this front-end's own sink |
| `take_settled()` | which commands the server answered, and how |

`refresh_lines` is the important shape: the front-end builds its own `Sink`
inside the method, so the client library never borrows a terminal type or holds
two mutable borrows of one view. In the TUI the impl sits in `run.rs` next to
`TuiSink` for exactly that reason.

## Why two traits

`window_refill.rs` has `WindowFacts`: read-only answers about what a frame drew
(`stamp_of`, `floor_of`, `room_of`, `drawn`). `WindowView` is the mutable
presentation half. They are deliberately not one trait and `WindowView` does
not extend `WindowFacts`: verdict 4 puts the journal in the client, so the
client itself will implement `WindowFacts` locally, while a front-end over the
pipe implements `WindowView` and holds no window facts at all. Both are local
interfaces, not a cross-process query protocol; `CursorMessage` still only
circulates inside the client.

## Not done yet

`interactive` in `run.rs` still owns the loop, `TuiSink`, `give_back`, the
`Step` enum and the dial future; `log.rs` / `panes.rs` still own the window
index and the ratatui folding. Lifting those is the next step, then the client
binary target and the pipe.
