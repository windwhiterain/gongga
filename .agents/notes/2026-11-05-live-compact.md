# Live compact experiment

## Current opt-in contract

- Live compact is disabled by default. The opt-in is an independent boolean,
  `live_compact_enabled` (env `GONGGA_LIVE_COMPACT_ENABLED`), never a token count.
- Disabled sessions receive no plan request, no compaction tool schema, and no pending
  compaction delivery. Existing transcript rows remain untouched.
- The compaction schema is never in the ordinary tool list. It is added only to a request
  made while a durable compaction request is outstanding, and withdrawn after its commit.
- Planning is explore mode's job. The first-round PLAN_NOTICE injection, its harness
  marker, and the user_texts harness filter are gone; appending a user row never
  establishes compaction state.
- The starting checkpoint opens at plan finish — the explore -> work transition — not at
  a turn end. Plan finish is the agent's own act inside its turn, so `start_live_compact`
  never refuses a running session: it takes the last persisted `context_tokens` as the
  baseline (zero stays NULL for the usage write to fill), and refuses only a lineage
  with no real operator row — failure refuses the mode change. Origin and baseline land
  in one transaction. With no origin yet, interval requests cannot be delivered, so a
  session that never planned is never asked to compact.

## Initial contract (historical)

- A separate token interval initially defaulted to 20,000; zero disables it. Trigger on the delta between the
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

- `live_compact_interval_tokens` is the threshold only: it defaults to 20,000 and a
  zero interval is refused while the feature is enabled, so 0 never doubles as the
  switch. File configuration overrides the default and the environment variable
  overrides file configuration.
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

## SPEC alignment (2026-11-05 second round)

The authoritative SPEC (verbatim, given in session; the checkout's SPEC.md is the
older text and was not edited) has four clauses, and the first round's contract
differs on two of them:

1. after first true user message, inject prompt to ask agent to give a plan.
2. after the first turn end, set the starting checkpoint of compaction.
3. after the starting checkpoint, every time reach a context increase limit, ask
   the agent to call live compact to compact the history since the last checkpoint.
4. live compact summary can reference meta that directly embed in the summary,
   creating a new checkpoint.

### Changes (commit d1b700d, plus the fmt commit on top)

- `begin_turn` appends one user-role plan-request row immediately behind the first
  genuine operator row, decided by the same `live_compact_origin_seq IS NULL`
  transition that establishes the origin, so it fires exactly once per fresh
  transcript (including after heir empties one).
- The starting checkpoint is established at a turn end, not by the first usage:
  new `live_compact_started` flag; the per-usage baseline write is gated on it,
  and `establish_live_compact_baseline` runs at every turn end (no-op once set),
  copying the latest absolute `context_tokens` when it is positive. Mid-turn
  usage can therefore never start the interval inside turn one, while
  post-compaction baselines still re-establish at the next usage mid-turn.
- Clause 3's interval mechanism is unchanged (delta between the provider's latest
  absolute input tokens and the current baseline, default 20,000, 0 disables).
- Clause 4: `meta::embed` inlines each `@file`/`@skill` mention's content into the
  `live_compact` summary through the session's own machine, reusing
  `mentions`/`content`; unresolvable references and pictures stay literal. The
  tool result carries the embedded text, so the committed checkpoint artifact is
  self-contained.
- Everything else (append-only raw rows, checkpoint transaction, projection SQL,
  pending-task drain, rollback) is untouched.

### Verification so far

- `CARGO_TARGET_DIR=$PWD/target/live-compact cargo check -p gongga-server`:
  exit 0, only the two pre-existing protocol `fetch_update` deprecations.
- `cargo test --workspace` after fmt: exit 0; all 12 test binaries report
  `ok` with 0 tests (the workspace keeps no Rust tests, and none were added;
  no selftest/probe scripts were created or modified either, per the user's
  ban on writing tests this round).
- `cargo fmt -p gongga-server`: reformatted only the new `PLAN_NOTICE` constant.

### Not yet verified

- No real process run yet: trigger-only-after-turn-end, plan row placement,
  embed behavior, and projection after a commit have only been read in code.
  The three 6f5f5ea Claw runs predate all of this and d1b700d, so they verify
  neither the old binary nor these semantics; a new claw A/B is the next step.
- Cancel/resume of a queued notice was not exercised; the drain logic itself is
  unchanged from the first round.

## Review fix round (2026-11-05 third round, review: cc35308-live-compact-review.json)

- Blocker: the fresh-lineage reset and plan row lived only on `begin_turn`; heir's
  lesson lands as a pending inject inside the running turn, leaving a stale origin
  that suppressed every post-heir compaction round. Fixed with `append_user_conn`,
  the single boundary both `begin_turn` and `deliver_pending` cross in their own
  transactions: empty transcript resets lineage, first row anchors origin, tagged
  plan row follows. Not a string special-case for heir — any first row after an
  emptied transcript (heir lesson, monitor/trigger inject) gets the same semantics.
- Regression: monitor `last_report` popped the plan row and would resend plan prose
  on a self-failed retry. The plan row now carries `"harness": "live_compact_plan"`
  in its stored display; `user_texts` skips harness-tagged rows (contract: harness
  notices are not operator messages). `wire_item` never maps the tag, so the model
  sees an ordinary user row.
- comments-check back to the 6047dcd baseline (174); the three d1b700d additions
  and one new inline note are within budget.
- Gates on f7fb1c0: build 0, fmt --check 0, comments-check 174==baseline,
  test-layout 0, skill_contract 0, `cargo test -p gongga-server` raw exit 0
  (0 tests), ship --check 1 (pre-existing drift, unchanged, see review JSON).
- Verify record: `.dev/verify/f7fb1c0-live-compact-review.json`.
- Still unverified by execution: the heir walkthrough itself, monitor retry, and
  cancel/resume; next step is a real claw A/B on this sha.

## Focused-review predicate fix (2026-11-05 fourth round)

The physically-empty test never fired on the real heir path: transcript.py
deletes `seq < kept.seq`, leaving the in-flight call, and its result lands
before the lesson does. The fresh-lineage predicate is now "no kind='text'
row survives", unified across all kinds: text is the only conversational
kind, the kept pair is not text, the plan row is (so a lineage never
re-resets), and the REQUEST_KIND marker is not. ship/heir untouched.

Gates on 607ea38: build 0, fmt 0, comments 174==baseline, test-layout 0,
skill_contract 0, cargo test -p gongga-server raw exit 0, and
scripts/heir_selftest.sh exit 0 (unmodified). That selftest covers the heir
script's copy/wipe/kept/rollback semantics against this checkout's schema —
which now holds the live_compact tables — but asserts nothing about
server-side lineage state; the post-heir live-compact state remains
code-review-only. Verify record: .dev/verify/607ea38-live-compact-review.json.
# Live Compact

Goal: implement the four Live Compact clauses in the user's modified `SPEC.md`, then run a real claw bench comparison on `lichen-vm`.

- Main worktree: `C:/resource/gongga`, main at `a3effa97`; user's `SPEC.md` modification and untracked cover image are not ours.
- Implementation writer: `live_compact/impl`, branch `feature/live-compact`, worktree `C:/resource/gongga/.worktrees/live-compact`, model `kimi-coding/kimi-for-coding`.
- Remote scout: `live_compact/bench_scout` on `lichen-vm`, model `qwen-dashscope/qwen3.8-flash`.
- `lichen-vm` runner greeting and server protocol check passed before delegation.
- Parent reviews the implementation, merges only after verification, follows `evolve` for server-code activation, then delegates the actual remote benchmark.
- No tests may be written. No SPEC edits. No new limits/budgets. No processes left running at turn end.

## Remote recovery

`lichen-vm` already has `6047dcd feat: add agentic live compaction` in its own `C:/resource/gongga/.worktrees/live-compact`; this is separate from the local worktree at the same path. The initial local `live_compact/impl` exploration was interrupted without edits. `live_compact/remote_impl` now owns the remote implementation audit.

The remote claw checkout is `/root/claw-swe-bench/.worktrees/gongga-live-compact` in WSL, at `dad4308`. Existing old-binary (`6f5f5ea`) single-instance results for `django__django-11880`, MiniMax-M3.1-Flash:

| arm | resolved | checkpoints | requests | output tokens | peak input |
| --- | --- | ---: | ---: | ---: | ---: |
| baseline | yes | 0 | 55 | 26,766 | 49,035 |
| 10k | yes | 10 | 99 | 115,980 | 37,614 |
| 20k | yes | 2 | 57 | 28,153 | 28,640 |

Those are historical evidence only; the revised current commit must be rebuilt and rerun. The benchmark adapter, venv, credentials and five local instance images are present. The scout's provider `qwen-dashscope` failed with HTTP 400 Arrearage and was marked out; scout completed on DeepSeek.

## Latest-SPEC alignment

The remote checkout's `SPEC.md` was stale and was not edited. The parent supplied the user's four current clauses verbatim. `live_compact/remote_impl` committed the alignment through code `607ea38` (HEAD `c5424e0`, documentation only): one tagged plan request after the first genuine operator row, a `live_compact_started` flag so the first baseline is set at turn end, direct file/skill content embedding in compaction summaries, and a unified fresh-lineage boundary covering both `begin_turn` and running injections. No tests or test scripts were added.

Independent review caught and required fixes for stale heir lineage state, monitor report selection, and three new comments violations. The final heir predicate is no surviving `kind='text'` row, not a physically empty messages table, because heir retains its in-flight call/result pair. Final focused review explicitly allowed the claw A/B. Gates on `607ea38`: server build 0, fmt 0, comments 1 with 174 == baseline, layout 0, skill contract 0, server tests 0 (zero tests), existing heir selftest 0 (7 checks; does not observe server lineage state). `ship.sh --check` has unrelated pre-existing drift (`82997a9` vs `f741281`); ship was untouched. Full Linux workspace build has an old unrelated TUI `cfg!` error; scoped cli/server build is 0.

The real three-arm claw run is `gongga-{base,live10k,live20k}-11880-607ea38`, same instance/model/prompt, three parallel arms with independent homes and ship caches. The WSL server binary sha256 is `3005ebdfa78d7118093a2e515ba57c49b7cad4d85f216a2057fa999d0b418759`. Results are stored remotely in `.dev/verify/607ea38-claw-swe.json`.

All three inference/evaluation pairs exited 0 and resolved `django__django-11880`. Base: 70 requests, 84 tools, 2,852,985 input, 39,534 output, peak 65,108. 10k: 23/29, 508,507 input, 14,481 output, peak 30,093. 20k: 50/61, 2,021,616 input, 36,610 output, peak 62,966. Every arm had the plan row at seq 2, no compact notice, and zero checkpoints: Claw's single operator message produces one turn, while the current SPEC starts the interval only after the first turn ends. Baseline and 20k each made one spontaneous live_compact call, correctly rejected because no durable request existed. Therefore token/time differences are single-sample model variance, not measured compaction benefit; comparison with the old implementation's 0/10/2 checkpoints is not a like-for-like compaction experiment. A multi-turn benchmark is needed to measure automatic compaction under this SPEC. The three runs shared the pre-existing future-commit GC timeout warning; no sanitization claim is made. Run homes/caches/containers/processes were cleaned and raw artifacts/final DBs retained.
