# Harness integration for declarative KV memory masks

2026-11-22 design proposal. No code or SPEC change has been made.

## User's direction

Like `live_compact`, the harness asks at a context-growth interval. The agent summarizes the
preceding segment into a memory block and receives an id. A memory provider evicts blocks under
pressure, using recent mean attention as a signal. The agent can explicitly `recall` selected
blocks for a short period.

## Confirmed first-version policy

The user chose harness-owned masking on 2026-11-22. The provider never changes visibility.
The harness limits how many memory blocks have their original spans unmasked; when a recall
would exceed that configured count, it masks the earliest block in the current unmasked set.
The numeric limit is live-tracked configuration, not a hard-coded agent choice.

“Earliest” means earliest **entry into the current unmasked set** (FIFO), not earliest block
creation. Ordering by creation would make an old block mask itself immediately on every recall.
Recalling an already unmasked block is idempotent and does not change its queue position.

The count covers historical blocks whose original spans are visible. Always-visible summary
artifacts and the current ordinary tail are not recalled raw blocks and are not in this count.
The first version has no separate recall lease: a recalled block stays visible until the count
policy displaces it. A `recall` result names both the blocks it made visible and any blocks the
policy automatically masked.

This is the only autonomous masking rule, and it is a harness rule. The view change, its cause,
and its generation are committed with the recall result before the next request. The provider
only translates the resulting view and offloads/restores residency.

## Recommended semantic model

There are two independent axes:

- **View**: visible or masked. This changes model semantics and is a durable harness decision.
- **Residency**: GPU, host, or absent-but-recomputable. This changes performance and capacity,
  never model semantics. The provider manages it.

Memory pressure must not silently change the view. A GPU eviction is a host offload; a visible
block that is needed again is restored before the forward that reads it. Raw transcript rows and
summaries are canonical, so their KV is always a recomputable derivative.

The tape is append-only and positions never move. `memorize` and `recall` change only the view.
Every committed view has a monotonically increasing `view_generation`; an LLM request pins both
`upto_seq` and `view_generation`, and every retry uses the same pair.

## Tools and checkpoint boundaries

Working names:

```python
memorize(summary: str) -> {id, source_range, view_generation}
recall(ids: list[str]) -> {recalled: list[str], view_generation, lease}
```

`memorize` reuses live compact's interval scheduler, backoff, and complete-ordinary-round boundary
selection. It does not reuse live compact's transcript cut:

1. The source is the span after the previous memory checkpoint through the newest complete
   ordinary round. The current incomplete round remains visible; a tool call is never separated
   from its result.
2. In one transaction, store the memory block, append the tool result containing its server-issued
   id, mask the source span for the next view, and advance `view_generation`.
3. The `memorize` call and result remain visible as the summary artifact. The summary therefore
   appears once, in the agent's own call, rather than being injected a second time.
4. The next checkpoint starts after that result. Source spans of successive memory blocks do not
   overlap.

`recall` unmasks the blocks' original source spans, not merely their summaries. It also advances
`view_generation`. Recalled blocks join the harness's unmasked FIFO; if the configured count is
exceeded, the harness masks the earliest entries before the next request. There is no separate
lease timer in the first version, and count eviction never rewrites an in-flight request.

Summary artifacts stay visible as the compact memory index. A later hierarchical summary can fold
those artifacts if their accumulated size becomes the next problem; that is not part of the first
implementation.

## Durable state

Canonical harness records are model-independent:

- `memory_block`: id, session, source seq range, summary call/result seq, creation time.
- `memory_view`: session, generation, pinned upto-seq, masked source spans, active recall leases.
- Optional observed statistics: per-block attention EMA, last visible step, last recall step.

Token ranges and physical KV block ids belong to the provider, not the database contract: they
change with tokenizer/model and engine layout. KV is also tied to the exact model; after a model
change or provider restart, the provider rebuilds it from the canonical transcript and view.

Live compact becomes a choice of projection strategy sharing the same checkpoint machinery:
`cut` for ordinary providers, `mask` for a provider that declares the capability. A standard
OpenAI/Anthropic endpoint cannot accept a mask; silently treating `mask` as a normal prompt would
be a semantic bug. Fallback to `cut` must be explicit.

## Request and engine path

The existing `RequestSource` already pins `upto_seq`; add the view generation to that snapshot.
For a mask-capable provider, send the full canonical tape plus a declarative view (memory ids and
visible/masked seq spans), not physical block ids. The provider translates seq spans to token
spans and then to its existing stable `mask_mod` closure. A new generation changes mask contents
and forces the block-mask rebuild; an unchanged generation reuses it.

The full raw input remains append-only, so provider prefix reuse remains possible. Mask changes
must not invalidate or rewrite the underlying KV tape. Newly masked spans are offload candidates;
newly recalled spans are restored at their original logical token positions before attention.

Autonomous provider **residency** changes need only statistics/deltas. Every **view** change is a
harness event stored and sent before the next request. That includes automatic displacement by
the unmasked-count policy as well as explicit `memorize` and `recall` calls.

## Attention signal: exact meaning and limitation

A useful per-block value is normalized attention mass per step, aggregated over the queries and
heads that were allowed to read the block, then smoothed over recent steps. It must not be an
unbounded sum over all historical queries, which mostly measures block age.

FlexAttention does not expose this for free. Returning a query-by-key attention matrix would
recreate the memory/bandwidth cost that block pruning avoids. The implementation needs a kernel or
backend auxiliary reduction that emits per-memory-block mass without materializing the matrix.

A masked block has zero subsequent attention by definition. Therefore attention cannot discover
that a masked block deserves recall; only explicit agent recall (or a separate semantic retrieval
mechanism) can do that. Keep `last_visible_attention_ema` for masked blocks and mark it stale.
For GPU-to-host eviction, recall leases and current visibility dominate; attention is at most a
tie-breaker/prefetch predictor. For host eviction, only drop KV that can be recomputed from the
canonical source.

## Context-window boundary

Masking reduces attention work and offload reduces resident KV. Neither moves RoPE positions nor
automatically removes a provider's maximum-position constraint. Long-tape operation beyond the
model's declared context remains a separate engine/quality question; the harness must not report
masked tokens as a smaller positional context.

## First implementation slice

1. Add durable memory blocks/views and the two tools, reusing checkpoint boundaries.
2. Add provider capability negotiation and the pinned view-generation request extension.
3. Connect view generations to the already measured vLLM mask patch; prove mask/unmask and retry
   equivalence before enabling offload.
4. Add residency transitions and resident-byte measurements.
5. Only then add auxiliary attention-mass collection and compare eviction policies.

Resolved by the user: the harness owns every mask decision. The first policy is a configured
count of raw-unmasked memory blocks with FIFO displacement. Provider pressure may still drive
GPU/host residency, but never visibility. Attention-guided eviction is deferred.
