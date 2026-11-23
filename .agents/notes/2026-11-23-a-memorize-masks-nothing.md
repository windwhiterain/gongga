# 2026-11-23 — a memorize registers a block; only the visible ceiling masks

User's ruling, verbatim in effect: `unmasked_ctx_limit` 不充足才是驱逐 memory block 的时
机；`memorize` 只登记块，不遮。上限本身跟 `live_compact` 同构地自适应（一次显式容量拒绝
减半，连续三次成功涨一档）。

## What the earlier run showed

A real mask-mode session on `minicpm47-35b-a3b` (`bench/dj16560`) committed two blocks and
masked both at once:

| | |
|---|---|
| `unmasked_ctx_limit` | 34732 |
| visible when block 217 committed | 10745 |
| span masked by that commit | 7596 tokens |
| `memory_blocks.unmasked_order` | `NULL` for both blocks |

So masking was a property of the checkpoint, not of pressure, and with 24K of headroom
left. Two consequences were measured on the same session: the agent re-read the file it had
just summarized (disk was the cheaper route back than `recall`), and `recall` was never
called even though both ids sat in its tool results.

The same run also exposed the interval side: `live_compact_baseline_tokens` was stamped
from the pre-mask measurement (10745) because a mask moved `view_generation` while the
stamp guard keys on `context_generation`, so the next ask waited for 10745+4000 instead of
the post-mask 3449+4000.

## What changed

- `memory.rs::memorize` no longer records a view event, advances no generation and clears
  no baseline; the block joins the tail of the unmasked queue (`unmasked_order`,
  `unmasked_count + 1`). `MEMORIZE_CAUSE` is gone with it.
- `enforce_limit_at_generation` stops at the first block the provider has not measured, and
  no longer fails when the visible count is still over the limit afterwards: the ordinary
  tail is not evictable, and a failure per step is not an answer.
- `recall` treats an unmeasured recalled block as an unknown projection rather than adding
  zero.
- `advance_generation` no longer touches the interval baseline, and `set_live_compact_baseline` keeps its one guard (`context_generation`) — the baseline belongs to the checkpoint, and a mask is not a checkpoint.
- Growth is counted on **the tape**, not on the visible set: `live_compact_measurement` reads
  `COALESCE(tape_tokens, context_tokens)` and the baseline is stamped from `input_tokens` in
  both modes. The older code stamped the visible count, so an eviction that hid 7596 tokens
  put the visible set 7596 behind the baseline it had just set, and the next ask waited for
  all of it to grow back. A mask changes the view and never the tape, so tape growth needs
  no reset, no guard and no special case.
- `SPEC.md` "Memory Mask" and the `memorize`/`recall` signatures; `docs/notes/server-live-compact.md`.

## L3 evidence (test home, this build)

A temp home, the new `gongga-server`, `ctx_limit 5%` (ceiling 5788), interval 1000, same
peer endpoint:

```
blocks   (1, span 0–23, token_count 5490, unmasked_order NULL)
         (2, span 23–29, token_count NULL,   unmasked_order NULL)
view     (1, mask, cause request, gen 1)
         (2, mask, cause request, gen 2)
visible  3082 → 3417 → 5548 → 8472 → [evict] → 3967 → 4302 → 6452 → [evict] → …
```

Both masks carry `cause = request`: the block was registered visible, and only the ceiling
masked it. Block 1's `token_count` was learned from the provider after it was masked
(5490), which is the mechanism that lets a later eviction project at all; block 2 was
masked by queue position with no measurement, as specified.
