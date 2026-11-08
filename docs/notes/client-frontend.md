# The Frontend interface and the lifted loop

## Where both steps landed

`FrontendEvent`, the `Frontend` trait, `give_back`, `Step`, `Dialing` and the
whole `run` loop now live in `crates/client/src/runtime.rs`. The loop reaches
its one UI only through `Box<dyn Frontend>`; no `Tui` type appears in the
client library.

`crates/tui/src/run.rs` is down to 159 lines: `TuiSink`, the three interface
impls and a thin `interactive` wrapper that builds the `Tui` and passes it to
`client::runtime::run` as a factory. `owner_pid` is supplied by that wrapper,
so the `--parent` verdict stays true when the client becomes its own process:
the UI will pass the pid it got in argv.

`log.rs` / `panes.rs` still own the window journal and the ratatui folding.
Everything else the loop needs is already in the client library:
`window_input.rs`, `window_sync.rs`, `window_refill.rs`, `window_state.rs`,
`window_cache.rs`, `window_index.rs`, `render_wire.rs`.

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

## What the client binary still needs

`crates/client` is still a library crate. Adding the bin target is the next
gate: it reads `UiIntent` frames from stdin, feeds them as `FrontendEvent`,
implements `Frontend` with a `WireSink` that writes `RenderMessage` JSON lines
to stdout, and passes as `owner_pid` the UI pid it got in argv. One client, one
UI, one pipe -- see `DESIGN.md` §1 and `client-render-wire.md`.

The `WindowFacts` half of `Frontend` is the remaining design debt: today the
TUI answers those queries from its journal. Once `log.rs` / `panes.rs` move,
the client implements them locally and a pipe front-end holds no window facts.
