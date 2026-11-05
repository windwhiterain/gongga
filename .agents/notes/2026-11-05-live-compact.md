# Live compact experiment

## Approved contract

- A separate token interval defaults to 20,000; zero disables it. Trigger on the delta between the
  provider's latest absolute `input_tokens` and the current checkpoint's post-compaction
  baseline, never on cumulative billed input.
- Create the first baseline in `begin_turn`, immediately after the first genuine operator
  message. Injected user-role rows do not establish it.
- At a step boundary, durably inject a request for the agent's `live_compact` summary.
- Persist the summary tool result and its checkpoint atomically. Never rewrite or delete
  raw messages for live compaction.
- A request retains the permanent prefix through the first operator message, every
  checkpoint summary, and only the latest one complete ordinary tool-call round plus
  subsequent rows.
- The `live_compact` call/result is the checkpoint artifact. The summarized-through cutoff
  is the latest complete ordinary round before the compaction request. Never select the
  compaction call itself as the retained ordinary round or emit an orphaned result.
- Preserve provider-required reasoning and call/result pairing. Stop rather than emit a
  malformed request if that cannot be established.
- Do not edit SPEC.md or tests, start a server, run evolve, or use a shared/live home.

## Bench reconnaissance

Claw-SWE-Bench was located at `/root/claw-swe-bench`. An isolated Claw worktree supplied
per-run homes and run-ID container namespaces; the original Claw checkout was not modified.

## Implementation WIP

- `live_compact_interval_tokens` / `GONGGA_LIVE_COMPACT_INTERVAL_TOKENS` defaults to 20,000;
  an explicit zero disables it. File configuration overrides the default and the environment
  variable overrides file configuration.
- `begin_turn` owns the genuine-message origin. Existing histories are not scanned or
  backfilled; an empty transcript after heir starts a fresh origin on its next begin turn.
- Delivery computes the newest complete ordinary round before appending the compaction
  request. Incomplete or ambiguous rounds suppress that request.
- Checkpoint artifacts contain the compaction call/result and all reasoning between the
  request and call. The latest ordinary round range contains its contiguous pre-call
  reasoning and complete call/result set.
- Request SQL retains the permanent prefix, indexed artifacts from every checkpoint, and
  the latest ordinary round plus subsequent rows. No message is updated or deleted.
- Commit clears the old measurement and advances the context generation, so usage from the
  request that ran `live_compact` cannot become the post-compaction baseline.
- `CARGO_TARGET_DIR=$PWD/target/live-compact cargo check -p gongga-server` passes. The only
  warnings are the two pre-existing protocol `fetch_update` deprecations. No server, home,
  end-to-end run, or new test has been used.
- Existing failed-attempt truncation now rolls back only checkpoints whose summary result is
  in that discarded suffix; otherwise a provider failure after the tool call could strand
  artifact references. Earlier checkpoints and all successfully sent history remain intact.

## Verification limits

- Server-only `cargo check` passes after the core and failed-attempt rollback changes.
- `cargo test -p gongga-server --lib -- --list` exits zero and reports zero tests; there were
  no relevant existing Rust tests to run, and no tests were added.
- Server-only Clippy finished with 20 existing server warnings and none naming live compact;
  the remote wrapper disconnected, so its process exit code was not captured.
- Repository-wide comments-check reports existing violations across 35 files. The new module
  initially had two long comments; after shortening them, its isolated comments-check exits
  zero. Unrelated files were not edited.
- `scripts/heir_selftest.sh` exits 2 because this worktree does not contain the ship
  submodule's `skills/heir/test_heir.py`. It did not start a server or exercise this feature.
- L3 evidence: the three-way `django__django-11880` Claw-SWE comparison used the Linux
  binary built from `6f5f5ea`, with live compact disabled and explicit intervals 10,000 and
  20,000. All three inference and evaluation commands exited zero and all three patches were
  resolved. They produced 0, 10, and 2 checkpoints respectively. Full commands, token
  trajectories, and evaluation reports are in `.dev/verify/6f5f5ea-claw-swe.json`.
- That L3 binary predates only the default change. Its 20,000 scenario set the environment
  explicitly, so it verifies the mechanism at the new default interval, not default-value
  resolution itself. Exact HTTP request bodies were not captured.

## Review fixes

- Failed-attempt rollback now rewinds one context generation per discarded committed
  checkpoint. If clearing nonzero context tokens fires the existing +1 reset trigger, the
  same update subtracts that trigger advance too, avoiding a double decrement or increment.
- A live-compact result is successful only when its checkpoint commits. Rejection or commit
  failure appends a new explicit `ERROR` tool result, finishes the call with `error` status,
  and emits error result/timing/UI metadata. Raw messages remain append-only.
- Each fix passed `CARGO_TARGET_DIR=$PWD/target/live-compact cargo check -p gongga-server`;
  only the two pre-existing protocol deprecation warnings remain. No tests were added and no
  server or evolve was run.
