# The Frontend contract for lifting the loop

## Where step 1 landed

`FrontendEvent`, the `Frontend` trait and `give_back` now live in
`crates/client/src/runtime.rs`. `crates/tui/src/run.rs` implements `Frontend`
for `Tui`, and `interactive` holds a `Box<dyn Frontend>`: every call in the
loop goes through the interface, and only the one line constructing `Tui`
still names the terminal. `Step` carries `FrontendEvent` instead of
`Option<UiEvent>`.

`TuiSink`, `Step`, `Dialing` and the loop body are still in the TUI. Everything
else the loop needs is already in the client library: `runtime.rs` (dial
cadence, spawn, ownership), `window_input.rs`, `window_sync.rs`,
`window_refill.rs`, `window_state.rs`, `window_cache.rs`, `window_index.rs`,
`render_wire.rs`.

Lifting `interactive` is the gate for the client binary: once the loop lives
in the client, a pipe-driven front-end is just another `Frontend`.

## The interface as landed

```text
enum FrontendEvent { Intent(UiIntent), Recomplete, Closed }

trait Frontend: WindowFacts + WindowView {
    fn next_event(&mut self) -> Pin<Box<dyn Future<Output = FrontendEvent> + Send + '_>>;
    fn closing(&self) -> Pin<Box<dyn Future<Output = ()> + Send>>;
    fn sink(&mut self) -> Box<dyn Sink + '_>;
    fn known_list(&self) -> Vec<Listed>;
    fn set_server(&mut self, server: Option<Server>);
    fn repoint(&mut self, home: &Path, endpoint: &str);
    fn restore_input(&mut self, text: &str);
    fn has_draft(&self) -> bool;
    fn server_is_ours(&self) -> bool;
    fn restore(&mut self) -> anyhow::Result<()>;
}
```

The first sketch of this note proposed a generic `F: Frontend` with a
`with_sink` closure. The landed shape is simpler, and the differences are
deliberate:

- **`sink()` returns `Box<dyn Sink + '_>`** instead of a reference or a
  closure. `TuiSink` borrows the `Tui`; boxing it gives that borrow an owner
  for the length of one call, so the client never holds a terminal type and
  `with_sink` is unnecessary. This also makes the whole trait object-safe.
- **`Box<dyn Frontend>` instead of a generic parameter.** Object safety lets
  `interactive` keep its signature; a generic `run` would have needed a
  `Session` struct to ferry ten constructed values into it. Trait upcasting
  (stable since rustc 1.86; this workspace is 1.98) supplies
  `&mut dyn WindowView` / `&dyn WindowFacts` for the sync and refill calls.
- **`closing(&self)` returns a `'static` future.** `tokio::select!` holds it
  beside `next_event()`'s `&mut self` future. `crate::closing::asked()` is a
  free async fn returning `()`, so the immutable borrow ends when the box is
  made; each select arm builds its own `closing` before the macro.
- **`FrontendEvent` replaces `UiEvent`.** `UiEvent::Intent(UiIntent)` is
  already the wire vocabulary, `Recomplete` stays terminal-only, and
  `Step::Input(None)` / `Step::Closing` both leave `Ok(())` -- so `Closed`
  covers the terminal going away.

Two borrow details worth keeping: a temporary `Box<dyn Sink>` inside
`if let Err(e) = client.handle_bytes(...)` lives until the end of the whole
`if let`, so the result is bound to a local first; and `TuiSink::say` became
`sink.note(kind, text)` -- both are the same `self.0.say(kind, text)` call.

`WindowFacts` and `WindowView` stay separate supertraits for the reason in
`client-window-sync.md`: the client will implement `WindowFacts` from its own
journal, a front-end over the pipe implements `WindowView`.

## Step 2

Move `interactive` and `Step` / `Dialing` into `client::runtime::run`. The
loop body no longer names `Tui`, so this is a mechanical move: `Tui::new`
becomes a factory argument and `tui::interactive` keeps only that call.

After that the binary target can read `UiIntent` frames from stdin, feed them
as `FrontendEvent`, and let a `WireSink` write `RenderMessage` JSON lines to
stdout. One client, one UI, one pipe -- see `DESIGN.md` §1 and
`client-render-wire.md`.
