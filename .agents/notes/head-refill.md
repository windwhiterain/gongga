# head-refill: head rows invisible in the UI after a handoff

Branch `fix/head-refill`, worktree `.worktrees/head-refill`.

## Problem

After `handoff` empties a session, the five head rows (instructions/1, tools/2,
agents_md/3, index/4, info/5) are deleted with the rest. The server re-stamps them on
the next request (`prompt_in_force` / `tool_set_in_force`), so the model still gets
them, but a loaded UI window never draws them again.

Root cause: the client assumes the transcript is dense (every seq `1..=end` exists).
Two places infer `held` from a range/watermark instead of actual rows:
- `client/lib.rs` fill completion: `held.insert_run(first, through)` (requested range).
- `client/lib.rs` `announce_transcript`: `held.insert_run(floor, seq)` (watermark).

After the cut, the recovery fill returns only the kept call + note (seqs 6,7) because
the head is re-stamped lazily *after* the note; the client marks `[1,7]` held anyway,
floor becomes 1, and `ask_older` never goes back. The silent re-stamp publishes no event.

## Fix (server interval event + client held-by-actual-rows)

- `protocol`: `Event::Transcript` gains optional `first: Option<i64>`. `None` = the
  watermark (as before); `Some(f)` = a *range write* `f..=seq` below the end.
- `server/agent.rs step_inner`: when the head was missing and `context_generation > 0`
  (a reset happened), after re-stamping publish `Transcript { seq: HEAD_SLOTS, first: Some(1) }`.
  Fresh sessions (generation 0) are not announced. `store::HEAD_SLOTS = 5`.
- `client/lib.rs`: `held` is written only by `hand_over` (actual rows); removed the two
  range-inference inserts. `announce_transcript` range branch sets `Sync.owed_range`;
  `ask_owed_range` fetches it as one Older page. `window_refill::answer_transcripts`
  fires `ask_owed_range` before the ordinary tail ask.

Traffic: one interval event + one `get_messages` range fetch per handoff; head content
travels once. No protocol bump (optional field is wire-compatible).

## Status

- `mbx build` green; `mbx fmt` applied.
- `mbx test -p gongga-client -p gongga-protocol --lib`: 7 passed.
- `mbx test -p gongga-server --lib head`: 2 passed.
- `comments-check`: no new violations vs the main baseline (remaining ones are
  pre-existing in `detached.rs` / `net.rs` / `runner.rs`). `test-layout-check`: 0.
- L3 `scripts/head_refill_selftest.sh`: PASS (real server + fake LLM + a loaded
  `gongga-client`). After a simulated handoff cut, the window fetches the re-stamped
  head as an Older page (seq 1,2,4,5). Result: `.dev/verify/<sha>-head-refill.json`.

Merged to main as `bd53ef20` (squash). The unrelated `title_note` WIP in the main
checkout was committed separately first (`cce591e9`), then this fix squash-merged on
top; `mbx build -p gongga-server -p gongga-client -p gongga-protocol` green and the L3
re-run PASS on the merged build. (A full `mbx build` cannot replace `gongga-tui.exe`
while the operator's window holds it -- os error 5 -- which is the running-exe lock,
not a compile failure.)

## Known related defect (not fixed here)

5 live sessions have duplicate seq 1 (`lichen-vm`, `Rusd`, `resume`, `merge`, `linux`):
an older handoff re-inserted the kept call at seq 1 (no HEAD_SLOTS floor), colliding with
the re-stamped instructions. Current skill floors at 6; the stored rows remain dirty.
