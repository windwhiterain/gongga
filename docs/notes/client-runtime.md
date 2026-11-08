# client/runtime.rs

The connection lifecycle and the front-end interface, lifted out of
`crates/tui/src/run.rs`. These are the parts that know nothing about terminals:/nthe reconnect ladder, whether this window starts a server at all, one dial
attempt, whose server answered, the `Server` enum, and `Frontend` /
`FrontendEvent` / `give_back`. See `client-frontend.md` for the interface.

## Why `owner_pid` is a parameter

`whose_server` used to compare the server's `--parent` against
`std::process::id()`. That was correct while the window *was* the process. The
2026-11-07 verdict makes `--parent` name the **UI**, with the client as its
child, so the library cannot answer "is this our pid" by looking at itself: it
would call a handover somebody else's server. The caller passes the UI pid.
`interactive` currently passes `std::process::id()` because the UI is still this
process; once the client is split into its own binary it passes the pid it got
in argv instead. The comparison itself did not move.

`Server::is_ours` still decides whether closing the UI stops the server, and
`Server::label` still spells the header's words. `crates/tui` re-exports the
enum, so `Screen::server` and `Tui::set_server` are unchanged.

## What is still in tui/run.rs

`TuiSink`, the `Step` and `Dialing` types, and the `interactive` loop body.
The loop now holds a `Box<dyn Frontend>`, so only its one `Tui::new` line names
the terminal. The input translation, `sync` / `settle`, `declare` / `answer_*`
and `give_back` have all moved into the client library. `log.rs` / `panes.rs`
still own the window journal and the ratatui folding. See `tui-run.md` for the
loop's invariants, `client-frontend.md` for step 2, and `DESIGN.md` §1 for the
stage order.

Nothing about behaviour changed in this move: the four constants, four
functions and the enum keep the same logic, with one more explicit argument.
`crates/client/src/server.rs:101` still refuses to spawn at a non-loopback
address, so none of this touches a remote shared server.
