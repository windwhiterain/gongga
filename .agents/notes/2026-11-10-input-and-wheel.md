# Completion and the wheel came back (2026-11-10)

All work happened in `.worktrees/client-split`. The main checkout was only read:
`git -C /c/resource/gongga status` showed the user's own `SPEC.md` work in
progress, and nothing here touched it.

## What the operator saw

1. Typing `/` showed no command candidates, and Tab did nothing.
2. After opening an event block, the wheel over it did nothing at all.

Both were diagnosed by running the real pipe path with `scripts/tui_live.py` and
reading the frames, not by reading code alone. Two of the three causes turned out
to be different from the first guess, which is why the frames mattered.

## Cause 1: a sequence split across two `read`s was typed as text

`vt_input::parse` treated a trailing `ESC` as the Esc key, on the assumption that
a console writes a sequence in one go. ConPTY does not: the raw log showed `ESC`
alone in one batch and `[<64;50;24M` in the next, so one wheel turn became an Esc
followed by twelve characters typed into the prompt (`> /set_render -[<64;50;24M`).

The reader now holds a trailing escape and waits 50 ms for the rest; only that
silence makes it the Esc key, and the remainder of an unfinished sequence is
dropped rather than typed. Reading blocks, so it moved to its own thread and the
parser waits on it with `recv_timeout`. Three tests were added, including the
exact split a wheel turn makes.

## Cause 2: the completion request was asked outside the loop that never returns

`pipe::run` checked `tui.input_line()` after its `select!`. `Tui::next_event()` is
a long-running future that returns only when it has something to say, and typing
does not make it return, so the check ran only when a frame happened to interrupt
it. The client only answers `Complete`, so both ends waited.

The request now lives at the head of `next_event`'s own loop, where every key and
every frame passes through: a line that changed since the last ask, or a pane
whose height changed, is asked about and returned as an intent. `pipe.rs` lost the
dead check and its `seen` bookkeeping.

Three follow-on fixes came with it:

- `Tui::refresh_completion` closes the popup instead of rebuilding it from the
  empty local completer, which used to throw away the answer the client had just
  sent.
- A Tab pressed before the answer arrived is remembered (`tab_asked`) and spent by
  the first `Completion` that lands, so a fast hand does not have to press twice.
- An answer whose range runs past the line is dropped as stale.

## Cause 3: a box that could not move ate the wheel

`Screen::scroll_at` handed the wheel to the box under the pointer whenever the
block was opened, and `Log::scroll_block` cannot move a body that already fits
under its cap. Opening a short event block therefore turned the row under the
pointer into something that could neither scroll nor let the pane scroll.

`Log::box_at` now offers the wheel only a box that has rows of its own. That is the
block's own property, not how far the gesture has already scrolled it, so one cell
always answers the same and no gesture changes hands halfway -- the reason the
handover was removed in the first place.

## Cause 4: a page of the past was one row

`WindowJournal::note_room` had no caller after A-4 deleted the frame hooks that fed
it, so `room_of` returned its default `1` and every `Older` page was a single row.
The drawing is the only thing that knows a pane's height, so `UiIntent::Room` was
added: the TUI reports each pane's height when it changes (not per frame), and
`WindowFacts::note_room` writes it into the journal.

Verified by a temporary `eprintln!` in `note_room`: the client logged
`room main = 21`, then `11`, `10`, `11` as the pane was resized.

## Evidence

Two scripts in `.dev/live/` (not tracked; they are reproducers, not gates):

- `diag-complete.script` — `/set_session main`, then `/del_session m` + Tab. The
  grid ends with `> /del_session main`, so the name came from the client's
  catalogue through the pipe. Exit 0.
- `diag-scroll4.script` — a log longer than its pane, resized to 100x20, wheel,
  `--block_size 2`, wheel, click a row, wheel. Frames appear after every wheel,
  with the `↓ bottom` badge, and the wheel after the click still moves the pane.
  Exit 0.

`--scenario window-state` still fails on the cover ("press any key" is drawn and
the scenario never dismisses it); that predates this change and the custom scripts
dismiss the cover first.

## Gates

`fmt`, `build`, `check --workspace` (14 warnings, all pre-existing in
`crates/server/src/tools/detached.rs`), `cargo test -p gongga-tui --lib` 10,
`-p gongga-client --lib` 7, `test-layout-check` 0, `skill_contract` PASS 8,
`ship.sh --check --local` in agreement, `git diff --check` clean,
`client_state_selftest` PASS 4s, `multi_client_selftest` PASS 13s.

`comments-check` returns 1 with 17 violations, all of them at the same counts as
`HEAD` (`git show HEAD:<file>` per file): `vt_input.rs` 5, `tui/lib.rs` 3,
`server/llm.rs` 3, `client/lib.rs` 3, `bundle/payload_impl.rs` 2,
`server/providers.rs` 1. New violations from this change: **0**.
