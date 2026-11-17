# A block keeps the ink it was born with (2026-11-17)

The symptom was an answer drawn in the quiet gray a folded reasoning row is drawn in:
lichen-vm's fifth-from-last output, seq 869. Five defects put it there -- four were found
and fixed earlier in the day, the fifth turned up while verifying the fourth.

## What was wrong

| # | where | what |
| --- | --- | --- |
| 1 | `server/src/llm.rs` | `reasoning_text` and `reasoning_summary_text` deltas shared one buffer, so a cut stream was stored as a `summary` and replayed in a field qwen's thinking mode ignores; the resumed step re-derived its whole plan |
| 2 | `server/src/store.rs` | `row_event`'s `_ =>` arm dropped `thinking_mark`, so a replayed transcript never showed the resume mark |
| 3 | `tui/src/log.rs`, `client/src/document.rs` | a stream-opened block and the document made from it were two blocks per key |
| 4 | `client/src/lib.rs` | a block's key was `(turn, step, part)` and its slot was retired only on `done`; a killed stream sends none, so the next row wearing the same labels appended into the dead block, which kept the ink it was born with |
| 5 | `server/src/anthropic.rs` | items were named `block-<index>` and `content_block_start` restarts that index at zero in every response -- so on this face the name of #4 came back every turn |

4's fix names a block after the provider's `item_id` (`ResponseEvent.item_id`). 5's fix
names an anthropic item `<message.id>-<index>`, counted when the stream never said one.
See `docs/notes/server-anthropic.md`, "an item is named after its response".

## What was observed, and what was not

- `python scripts/tui_live.py --scenario single-stream --attrs` on `e8c864fb`: 43 frames,
  every answer cell bright white (14 374 cells), zero plain white, reasoning dim italic.
  Data in `.dev/verify/e8c864fb-tui-log.json`.
- The killed-stream case itself is **not** observed in a window. It needs a stream cut
  before its `done` and then a second item in the same session; `tui_live_scenarios.py`
  has no such scenario, and a new one is a test.

## The check that was not checking

`tui_live_scenarios.py` asked the stand-in to call `bash`, which stopped being a tool when
the six shell verbs went (`d131e5f6`): the call answered `unknown command "bash"` in every
run. `expect hi-from-fake` matched only the frames in which the call's arguments were still
streaming -- a sealed tool result is a `summary`, so its body is folded away -- and that is
why the same scenario passed at 18:07 and timed out at 18:11 on one unchanged build. It
names `exec` and expects the call row `exec·bash` now.

## The same symptom, reported on `@monitor/providers` (19:59)

The store says the model thought once. Measured read-only on `~/.gongga/gongga.db`:
349 rows over 5 turns; the newest turn has 30 steps and 111 rows; **no `(turn, step)`
holds two `reasoning` rows or two `text` rows**; 83 distinct item ids and none appears in
two rows. The turn's log carries no `retrying`, no `filled a resume boundary`, and one
`why="interrupt"` (11:51:53, the previous turn, which ended `Cancelled`).

What the window draws with is older than the fix. Its harness is `5a35bd2b977f` (15:38),
whose `2e497e5d` and `ae036d14` are in but whose `8b79a436` (17:28, a block is named when
its item is created) is not -- and that is the commit this symptom lives in. The running
server is the image installed at 16:34 in `~/.gongga/bin`; the exe on disk was rebuilt at
18:13 by another checkout but no process is running it. A window keeps the build it
started with, so the fix arrives there only with an evolve.

## Two things a live run needs

- The worktree's `ship/` is a submodule whose `.git` points at a modules directory a
  worktree does not have, so `seed_ship_checkout()` cannot clone it and the server goes to
  the network. `rm -rf ship && git clone ../ship ship` (local) fixes it; `.gitmodules`
  carries `ignore = all`, so git does not report the difference.
- `.dev/live-home/ship` left by an interrupted run makes the next run refuse to start with
  `is not a clone`. Wipe `.dev/live-home` between runs.
