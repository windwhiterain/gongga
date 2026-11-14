# Taking over `monitor`: the reader is the next victim, and what is left

2026-11-14, session `monitor/2`. The session `monitor` and its subagent `monitor/data-repair`
both died with `HTTP 400 ... You have uploaded an unsupported image`, one after reading the
other's transcript. That is the whole failure mode of this bug: the poison does not need the
agent to be careless with pictures, only to *print* a transcript row -- a tool result row keeps
the reference it printed in its `display` column, and the harness attaches every reference a
tool prints (`agent.rs::attach_references`). So `monitor` died reading its own subagent's
report, exactly the way `escape` had died before it.

**The guard, for anyone taking a transcript apart:** query `gongga.db` and replace the
reference prefix in SQL or Python before printing. Never `cat` a transcript, never call
`get_messages` on a session that carries an image reference, and never paste a row back into
an answer that echoes it.

## What I verified

The chain (how a type is decided by extension, how an output's references are attached, how a
refusal was read as a retry) is in `2026-11-14-a-picture-that-is-not-a-picture.md`. The data
half is now done:

- `pictures` row 121 pointed at `.worktrees/uvpkg/.dev/img/a.png`, 28 bytes,
  sha256 `1e044b43...f80137`, and `<home>/pictures/121` was the same 28 bytes.
- Deleted: the copy, the source fixture, and the one reusable `uploads` row for copy 121
  (`https://api.deepseek.com/v1`, `file-api-4812a00b-...`). The `uploads` row is the part that
  is easy to miss: `files::mint` answers from it *before* it reads the copy, so deleting the
  bytes alone would have kept sending the provider's stale file id.
- `pictures` row 121 stays. That is documented as safe: a row with no copy degrades to no
  picture rather than to a wrong one (`image/pictures.rs`, `remember`).
- Proof, not code reading: `clone_session escape/uvpkg monitor/2/poison-check`, one message
  asking for a single word, answer `ALIVE` at seq 121. A request carrying the old rows went
  through. The clone was then deleted. No source fix has to land for those transcripts to be
  usable again: `llm.rs` already degrades a picture whose bytes are gone to a sentence.

## What is still running, and who owns it

| work | where | state |
| --- | --- | --- |
| image validation + refusal classification | worktree `.worktrees/image-refusal`, branch `fix/image-refusal` | alive and working; five files dirty, no commit yet |
| every exponential growth unbounded + retry visible in the session's UI | worktree `.worktrees/retry-unbounded`, branch `feature/retry-unbounded` | dispatched to `monitor/2/retry-unbounded` |
| merge both, run the gates, decide whether a handover is needed | here | me |

Two writers touch `crates/server/src/llm.rs`: `image-refusal` owns the classification region,
`retry-unbounded` owns the backoff region. That split is deliberate and is in the brief.

## Landed

- `a146b706` merges `fix/image-refusal` (a picture must be the format its bytes say; a refusal
  stated inside a 200 stream ends the turn), rebased onto `f14b6569` first.
- `1dbd1352` merges `feature/retry-unbounded` (one ladder in `crates/protocol/src/retry.rs`,
  every doubling uncapped, `Event::Log` carrying the session it is about so the retry line is
  drawn in that session's window), rebased onto `0aaccf82` first.
- Measured on `1dbd1352`: build, fmt, comments, test-layout, skill_contract, `ship.sh --check`
  and the server/client/protocol tests all exit `0`. `scripts/watchdog_selftest.sh` on a home
  with protocol-12 content: 8 of 9 scenarios, the one failure being a stale expected string in
  the script (`already running on this home` vs the server's `is already using this home`),
  which is red at `07426b9c` too. Record: `.dev/verify/1dbd1352-merge-takeover.json`.
- Both worktrees and the feature branch are gone; both branch probes are copied into
  `.dev/verify/`.

## Still open after this

- The watchdog's cap is gone, so a child that never lives 60 s climbs past any horizon and in
  practice stops being restarted. That is what "no ceiling" means and it mirrors
  `max_retries == 0`; whether the watchdog is the exception is the user's call.
- `ship/config.toml` in the content still says the delay doubles "up to 30s" and sets
  `retry_base_ms`, which is prose to fix through `scripts/ship.sh`.
- The live home's content checkout (`~/.gongga/ship`, `08854f0`) speaks neither `protocol` nor
  `python`, so a build that reads protocol 12 cannot install it. `evolve`'s `stage` syncs that
  checkout, so it must be run before anyone hands the home over.
