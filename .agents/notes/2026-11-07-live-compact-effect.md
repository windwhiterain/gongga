# Live compact: work origin and measured effect

The user rejected the earlier three-arm report as passive: all three tasks resolving did not explain why compaction never ran or prove its effect.

## Root causes

- Only explore -> work opened the starting checkpoint. A session created directly in work never obtained an origin or baseline, so token growth could never request compaction.
- Default work is stored as `sessions.mode = NULL`. The first work-origin fix therefore also needed to accept NULL, not only the literal `work`.
- The earlier experiment's claim that 41K--53K input had not crossed the 20K interval was wrong: the mechanism had not started.
- C remained explore because the benchmark never performed plan finish. A future plan-finish benchmark must explicitly issue that transition and verify its origin and baseline.
- A single large `read_file` spills to an output file and does not create the intended model-context growth. Parallel reads are also a poor compaction measurement because the latest complete ordinary tool round is retained. The useful mechanism experiment uses separate, bounded reads.

## Real-model experiment

Build: `dc8e01a` on `e3ed3117`. Model: `deepseek-official/deepseek-flash`; interval: 20,000 tokens. The enabled and disabled homes ran the same sequential reads and recall prompt.

The first automatic compaction reduced the next request from 24,158 to 5,078 input tokens (79.0%). The marker `IMPORTANT-MARKER-7719` survived. The inferred line count was not verified and is not presented as a recall success.

Across all ten phases:

| Metric | Enabled | Disabled |
| --- | ---: | ---: |
| Requests | 107 | 103 |
| Checkpoints | 6 | 0 |
| Cumulative input | 2,019,701 | 8,699,580 |
| Peak input | 34,691 | 168,855 |
| Cumulative uncached input | 331,125 | 165,948 |
| Cumulative output | 14,780 | 9,017 |
| Model latency | 153.705s | 139.896s |
| Cost at 2 / 0.04 / 8 CNY per million miss/cache-read/output tokens | 0.848033 | 0.745377 |

Live compact reduced cumulative input by 76.8% and peak input by 79.5%, but cost 13.8% more and accumulated 9.9% more model latency in this experiment. It is evidence for context control, not monetary savings. One model and one scripted workload do not support a statistical performance claim.

Complete measurements, usage rows, events, and database paths: `.dev/verify/dc8e01a-live-compact-effect.json` in this worktree and the main checkout.
