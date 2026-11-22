# Append is the only primitive

Date: 2026-11-22. Session `kv-mask`. User verdicts, verbatim:

> 这个框架将会不支持重新prefill, 因为一次decode生成的token区间一个mask，将过去的历史prefill会需要取历史中所有mask的交集，导致几乎没有mask，无法省显存，根据decode mask定制的硬件将无法同时装下非稀疏的所有kv做prefill

> 或者说可以re prefill，但必须分段prefill,每一段的mask都是相同或者交集依然很大的

This is a hardware-sizing constraint, not a reproducibility nicety. It settles the shape of
the framework. Nothing here is measured; the four-arm experiment that would settle it is in
`kv-mask-experiment-plan.md`.

## One operation

```
append(tokens, mask) -> append a segment of KV, positions unchanged
```

**"Prefill the history" is not an operation. It is a replay of appends.** There is no dense
prefill path, ever. This is the same shape as the SPEC's `context only has append/cut API`:
cut is a mask change, append is the only write. The proposal's own API (`append_spans` +
`active_mask`) already looked like this.

## The mechanism, stated precisely

The user's phrasing is "the intersection of all historical masks". The stronger and more
general version is about **the union of KV required within a prefill chunk**, and it does
not depend on what masks the kernel can express.

Query `p` in a chunk needs `M_{seg(p)} ∩ [0..p]`. The chunk needs the **union over its
queries**.

- **Homogeneous mask inside the chunk** (one `M_i` for the whole segment): the union is
  `M_i`'s active set — sparse. And once a key is masked for *every remaining query* in the
  chunk, it can be offloaded immediately: **there is a reclaim opportunity mid-pass.**
- **Heterogeneous masks inside the chunk**: early segments were appended with almost
  nothing masked, late segments with a lot masked, so the union ≈ the whole tape. **No key
  can be offloaded mid-pass**, because some remaining query always wants it. Peak resident
  = dense.

The intersection is the degenerate *safe* fallback: intersecting the excluded sets masks
only what was always masked, which is ≈ nothing, so it never removes a key some segment
needed — and it saves nothing. The union of excluded sets would compute *wrong* KV for the
early segments. Neither works. Hence segmentation.

**The load-bearing consequence: even a kernel that can express a per-query 2D mask is not
enough.** FlexAttention's `mask_mod(b, h, q_idx, kv_idx)` can express it, and the mathematics
then reproduces the tape exactly — but the union argument is about residency, not
expressiveness, so **the memory is still dense**. Arms 3 and 4 of the experiment separate
these two claims; that separation is the verdict on the whole argument.

## What must be durable: the mask log, not the KV bytes

Each segment's KV is a function of `(its tokens, the mask active when it was appended)`.
The current rendered view does not record that. Therefore:

- **KV bytes are an optimization.** Lost them → rebuild by segmented replay.
- **The mask log is essential state.** Lost it → the tape is *not reconstructible at all*.

```
durable state = (tokens, span boundaries, per-segment mask, offloaded KV bytes)
```

The SPEC's "everything must store and send" now has a concrete referent: the mask log must
be stored, and sent to clients whose cached state claims to mirror the session.

## The segmented-replay contract

- **Segment** = the maximal contiguous run of tokens appended under one mask. Segment
  boundaries = mask-change boundaries = span boundaries.
- **Chunk freely inside a segment** — it is homogeneous, so chunking is safe. Peak resident
  ≈ B + one chunk.
- **Active set ≤ a declared capacity B.** The server refuses a mask that exceeds it. Unmask
  is not free; it is capacity-limited.
- **B must cover the maximum active set over *all* segments, not just the current one.**
  Masks are not monotone — an unmask can make a later segment's active set larger than an
  earlier one's. Replay visits every segment, so the budget is a max over the log. Without
  this clause replay OOMs.
- **Offload is core, not optional.** The GPU holds B; the tape lives in host RAM/NVMe.
  Masking buys bandwidth and compute; offload buys VRAM. The claim "saves decode KV VRAM"
  needs both.
- A mask-optimized accelerator sized for sparse decode **cannot fall back to dense
  prefill**. So segmented replay must always be possible, which is why the log and the
  capacity contract are durability requirements rather than conveniences.

## Retracted: "a restart loses the ghost"

`2026-11-22-a-kv-mask-is-a-view-not-an-edit.md` previously carried this claim, and it is
**wrong under this design**:

> ~~A restart that re-prefills the current view computes the summary row without the ghost.
> The quality advantage and the restart-fidelity loss are one phenomenon. Text compaction
> is restart-faithful; a KV tape is not.~~

That holds only if a restart does **one dense prefill of the current view**. Under
segmented replay each segment is recomputed **under its own historical mask**, so the ghost
is reproduced exactly (within numerics). **This framework is more restart-faithful than text
compaction, provided the mask log survives.** The trade-off is void; do not reintroduce it.

## Cross-session sharing survives

The shared prefix was appended by the first session under `mask = {}`. A new session
replaying it uses the same `mask = {}` — homogeneous, so it is shareable and cheap.
Divergence starts only where masks diverge, which is the per-session tail that never shared
anyway. This is why the harness's shared cache tree (`docs/reference/cache-planning.md`,
keyed by the directory that shares bytes) is not threatened by mask-history dependence.

## See also

- `2026-11-22-a-kv-mask-is-a-view-not-an-edit.md` — view vs edit, literature map, the
  EVOKE/Leyline contradiction (now secondary).
- `2026-11-22-a-hybrid-state-is-a-free-ghost.md` — model choice; on hybrid the recurrent
  state is never masked and never changes, so unmask is exact there too.
- `kv-mask-experiment-plan.md` — the four-arm experiment that decides arms 3 vs 4.
