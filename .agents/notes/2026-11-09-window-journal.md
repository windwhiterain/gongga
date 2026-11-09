# A, step 1: the client owns the window facts

2026-11-09, `.worktrees/client-split`, branch `feature/client-split`.

## The verdict

Asked which way to take step 6, the user answered **A**: move the journal's data
layer into the client first, let `WindowFacts` be answered locally, and only then
make the TUI a drawing-only front-end. B (put `CursorMessage` on the wire) and C
(make the UI report viewport facts as a query) were rejected because each breaks
a verdict already made. Recorded in `DESIGN.md` §1, "裁定 A".

## Why A needs slicing

`log.rs` is not a data layer with a rendering layer beside it. `Entry` holds both
the journal facts (`id`, `window`, `key`, `text`, `slot`, `sealed`, `pending`) and
the ratatui fold state (`starts`, `col`, `width`, `styled`, `source`). `Log::landed`
and `Log::rewind` walk those folded bodies to build `Record`s. So "move 3000 lines"
is really "split `Entry`", which is A-2, not one mechanical copy.

A-1 takes the three facts that are already clean: `drawn`, `room`, `wants` in
`Panes`, plus the private `Facts` / `settled` in `crates/client/src/main.rs`. They
become `client::window_journal::WindowJournal`. `Panes` holds one and delegates;
`PipeFrontend` delegates its `WindowFacts` and `take_settled` to the same type.
One implementation instead of two.

## What changed

- `crates/client/src/window_journal.rs` (new): stamps, floors, drawn, room, wants,
  settled; `consume(CursorMessage)`, frame methods, getters, `forget`.
- `crates/client/src/lib.rs`: module declaration.
- `crates/client/src/main.rs`: deleted `Facts` and `settled`; `Layout.shown` now
  feeds `begin_frame` + `mark_drawn`, and cursor messages go to the journal.
- `crates/tui/src/panes.rs`: deleted `drawn` / `room` / `wants`; holds a
  `WindowJournal`; `forget` clears that window's facts too.
- `room_of` answers 1 when nothing was reported, matching `Panes` and
  `ask_transcript`'s own floor. The pipe front-end used to answer 0.

## Verification

Gates: `cargo check` / `build` / `fmt --check`, `comments-check` (one 122/120
preamble shortened), `test-layout-check`, `git diff --check`, `skill_contract`,
`ship.sh --check` local and remote all 0. L3 `client_state_selftest` PASS 4s.

`tui_live`: `window-state` 0; a short wheel-only script 0 (covers the `want`
path). `single-stream` returns 3 and **is not caused by this change**: the server
log says `provider roster read rosters=["C:\Users\17124\.agents\gongga-agents\providers.toml"]`
and `no provider "fake"`, so it fell back to `deepseek-official/deepseek-flash`
and `expect hi-from-fake` timed out. tui_live's generated fake roster is not being
read since main's provider work landed. Worth a separate look; it also means any
scenario that sends a message to the model cannot be used as a gate right now.

Test homes and scratch scripts were removed; the L3 watchdog tree was killed by
pid from that home's `server.lock`, never by image name.

## Next

A-2: move `Cache` and the per-window `Ledger` map into `WindowJournal`, split
`Entry` into a data block and a fold view, and make stamps and floors persist for
the pipe front-end. Then A-3 is the TUI spawn, with four control frames still owed.
