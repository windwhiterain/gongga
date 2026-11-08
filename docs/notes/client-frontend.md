# The Frontend contract for lifting the loop

## What is left in the TUI

`crates/tui/src/run.rs` still holds `TuiSink`, `Step`, `Dialing`, `give_back`
and the 240-line `interactive` loop. Everything else the loop needs is already
in the client library: `runtime.rs` (dial cadence, spawn, ownership),
`window_input.rs`, `window_sync.rs`, `window_refill.rs`, `window_state.rs`,
`window_cache.rs`, `window_index.rs`, `render_wire.rs`.

Lifting `interactive` is the gate for the client binary: once the loop lives
in the client, a pipe-driven front-end is just another `Frontend`.

## The interface shape

`interactive` touches `Tui` in four ways that a trait has to cover. The shape
below is the conclusion of the 2026-11-08 scout; it is a design, not code yet.

```text
enum FrontendEvent { Intent(UiIntent), Recomplete, Closed }

trait Frontend: WindowFacts + WindowView {
    fn next_event(&mut self) -> impl Future<Output = FrontendEvent> + Send;
    fn closing() -> impl Future<Output = ()> + Send;   // no self
    fn known_list(&self) -> Vec<String>;
    fn set_server(&mut self, server: Option<Server>);
    fn repoint(&mut self, home: &Path, endpoint: &str);
    fn restore_input(&mut self, text: &str);
    fn has_draft(&self) -> bool;
    fn server_is_ours(&self) -> bool;
    fn restore(&mut self) -> anyhow::Result<()>;
    fn with_sink<R>(&mut self, f: impl FnOnce(&mut dyn Sink) -> R) -> R;
}
```

Four decisions, each forced by something concrete:

- **Generic `F: Frontend`, not `dyn Frontend`.** `impl Future` and
  `impl FnOnce` in the interface are not object-safe. A generic `run` keeps
  the shape small; `tui::interactive` stays a thin non-generic wrapper that
  builds the `Tui` and calls it.
- **`with_sink` instead of `fn sink(&mut self) -> &mut dyn Sink`.**
  `TuiSink(&mut ui)` is a temporary wrapper over the terminal; a trait cannot
  return a reference to it. The front-end builds its own sink inside the
  closure, so the client never borrows a terminal type and never holds two
  mutable borrows of one view. `TuiSink::say` becomes `sink.note(kind, text)`
  there -- the two are the same `self.0.say(kind, text)` call.
- **`closing()` has no `self`.** `tokio::select!` holds the future from
  `next_event()` (which borrows `&mut self`) and the closing future at the same
  time. `crate::closing::asked()` is a free async fn returning `()`, so an
  associated function forwards it without a second borrow.
- **`FrontendEvent` replaces `UiEvent`.** `UiEvent::Intent(UiIntent)` is
  already the wire vocabulary; `UiEvent::Recomplete` stays terminal-only and
  maps to `FrontendEvent::Recomplete`. `Step::Input(None)` and `Step::Closing`
  both become `Closed` -- the loop already breaks `Ok(())` for both.

`WindowFacts` and `WindowView` stay separate supertraits for the reason in
`client-window-sync.md`: the client will implement `WindowFacts` from its own
journal, a front-end over the pipe implements `WindowView`.

## Two steps, each with its own gates

1. Define `Frontend` and `FrontendEvent` in `client::runtime`, implement it for
   `Tui`, and make `interactive` generic. The function body stays in the TUI
   and behaviour is unchanged: this is the interface-only step, verifiable by
   the two existing L3 scripts.
2. Move `interactive`, `Step`, `Dialing` and `give_back` into
   `client::runtime::run`. It no longer names `Tui`, so the move is mechanical.
   `tui::interactive` keeps only the construction of the view and the call.

After that the binary target can read `UiIntent` frames from stdin, feed them
as `FrontendEvent`, and let a `WireSink` write `RenderMessage` JSON lines to
stdout. One client, one UI, one pipe -- see `DESIGN.md` §1 and
`client-render-wire.md`.
