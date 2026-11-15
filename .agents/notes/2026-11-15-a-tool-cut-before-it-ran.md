# A tool cut before it ran

`lichen-vm` sent a `gongga_cli` `send_message` to a subagent and got
`{"exit":null,"id":null,"matched":false,"output":"","time":"0.000s"}` back in
0.000 s. The model read that as a failure and sent the same message again. Neither was
true: the command never ran, and the answer never said so.

## What the ledger shows

`~/.gongga/gongga.db`, session `lichen-vm` (id 256): 12 of its 262 `gongga_cli` calls
came back that way, and all 12 are the first tool of a turn's first step; none of the
224 later-step calls did. Each failing call is followed 0.02--1.4 s later by its own
step's `usage` row, whose `gap_ms` equals the age of the row before it (431 s, 520 s,
762 s ... 2248 s): when the tool ran, the last `usage` row was still the previous
turn's. Its message landed once, from the retry: `lichen-vm/gaps` carries one row, not
two.

## Two defects behind one symptom

- `agent::run_before_cache_expiry` measured the tier's deadline as
  `window * (100 - margin) / 100` since the last `usage` row, and at `remaining == 0`
  fired the tool's child token *before* awaiting it. That row is appended when the
  stream completes, which is after the step's tools have run, so a turn starting after
  the window went cold read `remaining == 0` for every call of its first step and
  cancelled each one before it started. The design assumed a tool already waiting.
- `tools::run_command` answered its cancelled select with an empty `Result` and
  `ToolOutcome::ok`: `exit: null`, no output, status `ok`. Silence passed for success,
  the one thing `BashTool::run (a cancelled result)` is documented never to do.

`exec` is cut the same way when the condition holds (empty window, `exit: null`,
70--130 ms), so its program starts and the call reports nothing.

## What changed first, and then what the maintainer ruled

The first fix kept the cut and made it honest: a deadline already spent ran the call
whole, and a cut `run_command` answered an error instead of an empty `Result`.

Then the ruling: **delete the feature -- `keep_alive` already covers it.** `keep_alive`
is what keeps the prefix warm: one heartbeat per turn at
`window * (100 - margin_pct) / 100`, and the request it starts is the refresh. The tool
cut was a second answer to the same problem and the one that could only lose: a tool cut
mid-wait still has its job running, while a tool cut before it starts has done nothing
and says so only if someone invents that sentence.

So `run_before_cache_expiry` is gone, and the per-call child token with it: `ctx.cancel`
is the turn's own token now, so the only cut a tool can see is the turn ending. What
stays from the first fix is `run_command`'s answer, which is exactly that case.
`cache_fire_after_ms` and `[model] cache_margin_pct` stay, with one reader left:
`commands::arm_keepalive`.

## Verified

- `cargo build` exit 0, 39 warnings and none new (the one near this code is
  `session.rs:1418`, which predates the change); `cargo fmt --check` exit 0.
- `cargo test -p gongga-server --lib`: 28 passed, 0 failed.
- `comments-check`: agent.rs 4 -> 4 and tools/mod.rs 5 -> 5 violations, so no comment
  this change adds goes over the 120-character preamble budget; `test-layout-check`: 0
  violations; `scripts/skill_contract.sh`: PASS; `scripts/ship.sh --check`: protocol
  13 == 13 and published. That last one ran in the main checkout: a fresh worktree has
  no `ship` checkout, and `--check` there exits 1 on the missing checkout, not on a
  drift.

## Open

- A turn blocked inside one long tool wait no longer gets a heartbeat: `arm_keepalive`
  skips a busy session, so the prefix can go cold there and the next request is a cold
  one. That is the price of deleting the cut, and it is the right way round -- the wait
  belongs to the tool.
