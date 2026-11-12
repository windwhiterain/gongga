# Live compact: why the bench effect is poor (diagnosis)

Read-only research on lichen-vm (`Admin@10.144.144.2`) bench artifacts, cross-checked
against source in `.worktrees/live-compact-effect-3`. Builds on
`2026-11-07-live-compact-effect.md` and `2026-11-07-live-compact-ratio.md`; does not
restate the fixes those notes already landed.

## Already fixed / decided (do not re-diagnose)

- Cache break from the `live_compact` schema entering/leaving the tools array each
  checkpoint: fixed by the stored tool set ("stable head"). Synthetic cost
  1.138x -> 0.683x vs disabled (`2026-11-07-live-compact-ratio.md`).
- Unasked-for `live_compact` call: now a no-op; was an ERROR that invited retries.
- Work-born sessions never compact: intentional. explore -> work at plan finish is
  the only door; `sessions.mode = NULL` is work.

## Still-open cause: checkpoint-induced reconsideration (real tasks)

Evidence: `.dev/verify/6f5f5ea-compact-confusion.json` (django__django-11880,
deepseek-flash, build 6f5f5ea), per-checkpoint transcript inspection:

| arm | requests | output tokens | peak | checkpoints | first fix edit |
| --- | ---: | ---: | ---: | ---: | --- |
| base (no compact) | 55 | 26,766 | 49,035 | 0 | seq 77 |
| live 10k | 99 | 115,980 | 37,614 | 10 | seq 222 (6 checkpoints before any edit) |
| live 20k | 57 | 28,153 | 28,640 | 2 | seq 69 |

- The compact calls themselves cost only 6,638 of the 10k arm's 115,980 output
  tokens. Summary generation is NOT the cost.
- Each summary keeps "Remaining: implement the change" plus a design alternative,
  and the design oscillates across checkpoints (shallow-copy -> value-copy ->
  independent dict -> fallback-to-shared -> the eventual deepcopy). After each
  checkpoint the agent re-opens settled deliberation: re-reads the same Field
  source, re-searches upstream tickets, re-defers the edit (seq 151 = 36,538 chars
  "Now let me implement the fix" but reopens alternatives; seq 270 "implement from
  reasoning" after the fix is already verified).
- Damage scales with checkpoint COUNT, not summary cost: 20k (2 checkpoints) ~ base;
  10k (10 checkpoints) = 4.3x output, 1.8x requests.

### Mechanism in the summary contract (still present in effect-3)

`crates/server/src/tools/live_compact.rs`, `summary` parameter:
"What a later turn cannot recover ... the decisions, the paths and identifiers the
next step needs, **the open question**, and the facts it depends on. The server keeps
... **every earlier summary verbatim**, so do not restate them."

- It asks to preserve "the open question" but never asks to mark a decision SETTLED
  or to commit to a chosen design.
- "every earlier summary verbatim" means open questions accumulate across
  checkpoints and are never retired.
- Net: the design space stays open and the model re-deliberates after each
  checkpoint. The synthetic read+recall bench cannot see this (no design
  deliberation), which is why it reports "context control, recall preserved" while
  real implementation tasks regress.

## Economic framing (secondary, mostly addressed)

`.dev/verify/dc8e01a-live-compact-effect.json`: with compaction working, cost +13.8%
/ latency +9.9% despite a 76.8% input cut, because cached input is 50x cheaper than
misses (0.04 vs 2.0 CNY/1M) and compaction doubles uncached tokens (331,125 vs
165,948). The stable-head fix removed the tool-set-induced breaks; the remaining
uncached growth is the prefix rebuilt after each checkpoint. Recorded conclusion:
"context control, not monetary savings."

## Task-dependence (newest bench)

`.dev/verify/be4ab91-live-compact-bench.json` (sympy__sympy-13091, 2026-11-10):
the 10k arm compacted twice and still resolved; the failed 20k arm never compacted
(shell outside the instance container, /testbed empty). "compaction did not cost the
agent the task." The regression is worst on deliberation-heavy tasks
(django-11880), mild on mechanical ones (sympy-13091).

## Pointers

- Verify JSONs: `C:/resource/gongga/.dev/verify/` on lichen-vm
  (6f5f5ea-compact-confusion, dc8e01a-live-compact-effect, be4ab91-live-compact-bench,
  b2ded40-claw-plan-finish, cc35308-live-compact-review).
- SWE-bench runs on Linux `/root/claw-swe-bench` (docker, swebench-venv) via the claw
  adapter; lichen-vm holds the scripts and artifacts.
- Source: `.worktrees/live-compact-effect-3/crates/server/src/{live_compact.rs,
  tools/live_compact.rs,store.rs}`.
