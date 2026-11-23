# A mask request's tape end, and the reasoning item that stops it

Two defects were found on 2026-11-23 while answering a provider monitor for sessions
`probe` and `benchmark` (both `kvmask-peer/minicpm47-35b-a3b`, mask mode).

## 1. Fixed: the tape end named a row no request carries

`seq` counts published events, and the harness's own bookkeeping is one of them: a
`notice`, a `live_compact_request`, a builtin call it cannot replay. Those rows have no
wire form (`wire_item` answers `None`, so `content` is `NULL`), and no request sends them.
A mask request stated its end as `store.last_seq`, which counts them — so when the
live-compact ask left a marker row newest, the request claimed an end no item held and the
provider refused it: `gongga_memory requires every seq through upto_seq to be annotated`.

The adapter's rule is narrower than its wording: it only requires `max(annotated) == upto_seq`
(interior holes are accepted, a masked tail is not). The harness must therefore state the
end it actually carries. `MemoryView::upto_seq` now does, measured by `store::SENDABLE_ROW`
— the same predicate the transcript walk reads, so the set and the end cannot drift apart
again. `RequestSource::upto_seq` keeps `last_seq` on purpose: it is also where a failed
attempt's rows end, and `truncate_after` would otherwise delete a marker row.

## 2. Open: a reasoning item that is not last fails the adapter's marker mapping

With the annotation fixed, the next mask request still fails —
`memory marker rendering did not preserve exactly one marker`. Measured against the
endpoint directly: the trigger is a `reasoning` item followed by any other item. The same
items with contiguous seqs fail; dropping the reasoning item succeeds; a reasoning item as
the last item succeeds. The adapter marks `reasoning.content` with a nonce and requires it
to survive a re-render, and vLLM's template does not replay reasoning text into the prompt
when something follows it.

Both halves are defensible: the harness stores and sends what the provider returned, and
the endpoint cannot map a span it does not render. Resolving it is a decision, not a patch —
either the endpoint skips spans for items its renderer drops, or a mask endpoint stops
receiving reasoning items. Until then, mask mode on this endpoint fails on the first
multi-item turn even with defect 1 fixed.

Measurements: `.dev/verify/d67555a4-mask-tape-end.json`.
