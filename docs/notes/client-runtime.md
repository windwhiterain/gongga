# client/runtime.rs

The connection lifecycle, the front-end interface and the whole interactive
loop, lifted out of `crates/tui/src/run.rs`. These are the parts that know
nothing about terminals: the reconnect ladder, whether this window starts a
server at all, one dial attempt, whose server answered, the `Server` enum,
`Frontend` / `FrontendEvent` / `give_back`, and `run` itself. See
`client-frontend.md` for the interface.

## Why `owner_pid` is a parameter

`whose_server` used to compare the server's `--parent` against
`std::process::id()`. That was correct while the window *was* the process. The
2026-11-07 verdict makes `--parent` name the **UI**, with the client as its
child, so the library cannot answer "is this our pid" by looking at itself: it
would call a handover somebody else's server. The caller passes the UI pid.
`interactive` still passes `std::process::id()` because the UI and the client
share a process there; the `gongga-client` binary passes the hidden `--parent`
it got in argv. `run` also hands that pid to `spawn()`, so the server follows the
UI and survives a client that dies while its window stays. The comparison itself
did not move.

`Server::is_ours` still decides whether closing the UI stops the server, and
`Server::label` still spells the header's words. `crates/tui` re-exports the
enum, so `Screen::server` and `Tui::set_server` are unchanged.

## A server that will not leave

`server::stop` answers `Result<Option<u32>>`, not just the old pid. `Ok(Some)`
names the server that left, `Ok(None)` means nobody was serving, and `Err` means
cancel and kill both failed: that server still holds `server.lock`. `run` reports
the error through the front-end and starts nothing, because a replacement would
refuse the lock and the window would end up with no server at all. Porting this
branch from `tui/run.rs` was the only semantic work in merging main into the
client split; the rest of that file had already moved here.

## What is still in tui/run.rs

`TuiSink`, the three interface impls (`Sink`, `WindowView`, `Frontend`), and a
thin `interactive` wrapper that constructs the `Tui` and hands it to
`runtime::run` as a factory. `Step`, `Dialing`, the loop body, input
translation, `sync` / `settle`, `declare` / `answer_*` and `give_back` all live
here in the client library now. `log.rs` / `panes.rs` still own the window
journal and the ratatui folding. See `tui-run.md` for the loop's invariants,
`client-frontend.md` for the interface, and `DESIGN.md` §1 for stage order.

Nothing about behaviour changed in this move: the four constants, four
functions and the enum keep the same logic, with one more explicit argument.
`crates/client/src/server.rs:101` still refuses to spawn at a non-loopback
address, so none of this touches a remote shared server.
