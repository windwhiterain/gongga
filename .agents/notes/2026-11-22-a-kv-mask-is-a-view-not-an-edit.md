# A KV mask is a view, not an edit

Date: 2026-11-22. Session `kv-mask`. Source: the user's proposal in `C:\桌面\kv-mask.txt`
(their own question, followed by an LLM's endorsement of it), read against three 2026
papers fetched this session. **Nothing here is measured on our hardware.** Paper numbers
are self-reported; both papers state their experiment scripts were AI-written, and neither
is peer-reviewed (arXiv / Zenodo). Treat every number below as a claim to reproduce.

Raw fetched text: `.dev/kv-mask/refs/{leyline,evoke}.txt` (scratch, cleanable; the URLs
below recover it).

## The proposal

The user's own words, the file's first paragraph:

> 既然现在稀疏注意力大行其道，我能不能提供一种特殊的推理服务，允许用户的 harness 给定本次请求的
> kv mask，推理时只装入没被 mask 的 kv cache（位置编码不变）。这样 harness 既可以自由的组合上下文
> （保顺序），又能不破坏 kv cache？

Four commitments, and they are the whole design: the mask is a **per-request parameter**;
only unmasked KV is **loaded**; positions are **unchanged**; the tape is never broken.

## The distinction

The 2026 literature on "let the harness control the KV cache" is built on **imperative
mutation of a mutable cache**. The proposal is a **declarative per-request view over an
immutable tape**. That is not a naming difference; every other difference derives from it.

| axis | the proposal | Leyline | EVOKE |
| --- | --- | --- | --- |
| operation | view: which spans this request activates; tape untouched | edit: `(s_start, s_end, R, m)` replaces a span with `R`; the cache is changed from then on | evict/recover: cells freed, bytes moved to host RAM, spliced back |
| positions | unchanged, by construction | reindexed by `Δ = \|R\| − (s_end − s_start)`, downstream `K_pe` rotated | `seq_add` closes the gap; recovery re-anchors to a **new** position |
| who decides | harness, semantically, per request | policy, per turn (signal-agnostic) | **engine-side scorer** under budget pressure; harness only supplies `evoke_priority` / `evoke_pinned` priors |
| reversibility | flip a bit; bytes never moved, positions never changed → **exact restoration of the original view** | no recovery path in the directive; `forget` re-prefills | reversible, but the block lands at a new position, not the original view |
| what is saved | attention bandwidth/compute ("only load unmasked") | re-prefill compute. Explicitly: **"KV-pool occupancy is not the figure of merit"** — spliced slots stay trie-pinned, peak `available_size()` does not drop | lifecycle cost 5.9–7.5× below re-prefill (Qwen2.5-7B); wall-clock is truncate-parity, explicitly **"Not a speed win"** |
| architecture | argues native block sparsity (MoBA/NSA) makes it in-distribution | **MLA only**; §6 is an explicit invitation to extend to GQA/MHA | dense GQA / hybrid Mamba / MoE, on a llama.cpp fork |

KVTether (arXiv:2609.39819) is a third axis again: KV **lifecycle states** (birth, death,
idle, reuse) and capacity pressure among live-but-idle KV, exported from message-level
events. It is not about what one request sees.

## Three consequences worth keeping

**1. View semantics has no chained-rotation drift, and its unmask is exact.**
Leyline's Numerics limitation says it has *not* measured drift where "the same physical
slot is re-rotated thousands of times per session", and recommends periodic re-prefill for
such deployments. A long-lived gongga session with many checkpoint generations is exactly
that deployment. Under the proposal positions never change, so the failure mode does not
arise. EVOKE's recovery is "same bytes, new slot": repeated evict/recover accumulates
rotations, and EVOKE itself measured that placement matters (tail re-anchoring helps only
near a long-context model's far edge, is neutral at normal distances, and **hurts
short-context models**).

**2. Preserve-mode consumes the position budget monotonically — but that is not the wall
EVOKE cleared.** (Corrected 2026-11-22: the original wording conflated two different walls.)
Positions unchanged means a 100K-token tape with 5K active still places new tokens after
position 100K. Two separate bounds follow, and only one of them is hard:

- **KV-capacity wall** — EVOKE's "66K-token session under an 8K KV budget completes 150
  sections while no-eviction dies at the 32K wall at section 74" is a *KV allocation* wall
  (llama.cpp's `n_ctx`), not a position limit. **Offload clears it without compaction**:
  under the segmented-replay contract the GPU holds the active set B and the tape lives in
  host RAM, so preserve-mode never hits it. See
  `2026-11-22-append-is-the-only-primitive.md`.
- **The model's native position ceiling** — 262144 on both chosen models. Preserve cannot
  reclaim this one, and it is a much looser bound than VRAM. Compaction (`rerotate`) is the
  only way past it.

So the synthesis is narrower than first written: **view semantics decides what a request
sees; offload decides where the KV lives; compaction is needed only when the position
ceiling itself is reached** — `rope_mode` as a per-directive choice, not a global default.

**3. "Several orders of magnitude" is not supported by either paper.**
On a prefix-cache hit TTFT is already near zero, and **masking a trailing span is already
free today** (a shorter prompt is a prefix of the cached longer one, so the radix lookup
hits). The unique value of a mask is **removing a middle span while keeping the suffix**.
Both papers report avoided re-prefill compute, not memory and not dramatic speed. Also:
`soft_mask` saves bandwidth, not VRAM — the pages stay resident.

## The contradiction between the two papers (secondary; unresolved, cheap to settle)

EVOKE, "Why eviction doesn't corrupt the cache":

> After the shift, `Q_new · K_survivor` returns the same relative-position dot product the
> model would compute if `house is green in color.` had never been decoded. **The model
> behaves identically to one that read the truncated sentence directly**: information
> loss, never corruption.

Leyline §3.1 says the opposite on purpose, and §4.1 constructs a case that separates them:

> The amortize-mode contract is **positional, not informational** … `K_pe` and `K_nope`
> are preserved (they were computed under attention to the original chunk during prefill,
> and **that attention is exactly what we want to keep**) … This is **deliberately weaker
> than "equivalent to re-prefill of the substituted prompt"**.

Leyline's microbenchmark: a 57-token calculation buried mid-prompt, wrapped by 5 pre-tokens
and 9 post-tokens, replaced by an 11-token `[truncated]` stub, `Δ = −46`. Greedy first
decoded token: **full-context → `34` (correct), re-prefill of the stub prompt → `0`,
Leyline splice → `34`**. bf16 weights, stock HF forward, three paths sharing model and
tokenizer state.

Both describe the same mechanism (`seq_rm` + `seq_add` + deferred RoPE shift). The
positional claim is true; the behavioural claim is not. **Reading**: EVOKE's sentence holds
only when the survivors do not depend on the evicted content — its example is two unrelated
sentences — while Leyline's is constructed for strong dependence. **Neither paper measures
the gradient.** That gradient is the experiment: dependence strength → divergence between
the splice view and a re-prefill of the same view. It also lands in Leyline's explicitly
open GQA/MHA coverage.

This is the same phenomenon as gongga's own `live_compact`: a checkpoint summary row was
computed with the folded segment visible, so the tape carries a "ghost" of it. See
`2026-11-05-live-compact.md`, `2026-11-13-live-compact-landing.md`.

**Retracted here (2026-11-22).** This note previously concluded that a restart therefore
loses the ghost, making the quality advantage and a restart-fidelity loss one phenomenon,
with text compaction restart-faithful and a KV tape not. That is wrong under the
segmented-replay contract: a restart replays each segment **under its own historical mask**,
so the ghost is reproduced exactly. The framework is *more* restart-faithful than text
compaction, provided the mask log survives. Derivation and durability requirement:
`2026-11-22-append-is-the-only-primitive.md`. Do not reintroduce the old trade-off.

## What is already published — do not claim it as new

- Ghost-as-feature for folding, and "erasure must take the downstream too" → Leyline's
  `amortize` / `forget` split, exactly. `forget` routes to prefix-trimmed re-prefill and
  exists for redaction, retention-mandated deletion, and correcting a misleading tool error.
- Identity-named spans + a stateful OpenAI-compatible endpoint + harness pin/priority tags
  → EVOKE has all three: `file:config.py#0`, `tool:search#3`, persistent `/v1/chat/completions`
  sessions decoding only the new tail, `evoke_priority` / `evoke_pinned`.
- VRAM ↔ host RAM ↔ disk tiering → EVOKE's host pool with `kv_restore_ram_budget_bytes`
  and `kv_restore_spill_path`. Cost: Qwen2.5-7B at `block_size=128` is 7 MiB/block,
  ~7 GiB per 1000-block session.
- Hollowing / keeping anchors → EVOKE's sink protection and `breadcrumb` backend (a compact
  marker: key, token count, eviction step); Leyline's stub `R`.
- Harness-side tricks that never touch the engine (byte-stable tool masking, append-only
  history, fork instead of edit, one thorough compaction) → already the gongga approach;
  see `docs/reference/cache-planning.md` (`index_change_tolerance`, `reference_revision`)
  and `2026-11-04-prefix-cache-simulation-and-explicit-tiers.md`.

## What is genuinely the proposal's own, and untested by both

1. **Per-request `active_mask` over an immutable tape** (view semantics itself).
2. **Preserve-with-holes as the primary mode.** Both papers compact; nobody has measured
   quality or bandwidth with the holes left in place.
3. **Composition with a native block-sparse router** — the harness mask as a user-defined
   prior mask layered on MoBA/NSA selection. Leyline is MLA (a compression architecture,
   not block routing); EVOKE is dense/hybrid/MoE. Neither evaluates a native block-sparse
   model. Out of reach on an 8 GB card (NSA/DSA models are large); theory plus a small
   proxy only.
4. **The segmented-replay contract, and the mask log as essential durable state.** Neither
   paper keeps a per-segment mask history, because both mutate one cache instead of keeping
   a tape of views. Recorded in `2026-11-22-append-is-the-only-primitive.md`; it is the
   increment the four-arm experiment is built to decide.

Engineering consequence of (2): "preserve + only load unmasked" is bandwidth-optimal only
on a **paged** substrate (filtering a block table really skips reads; a unified contiguous
cache with holes masks but still reads). That is precisely where EVOKE's vLLM v1 port
failed: the primitive composes from `swap_blocks_batch` + `rotary_embedding` with no CUDA
work, but **the policy layer does not transfer, because the V1 scheduler has no
session-scoped logical position space**. The proposal's distinctive mode is also the
hardest one to land on a production engine.

## User verdicts (2026-11-22)

> 我觉得他们的路线和我完全不同吧，不要再套用他们的原语了。

So: no EVOKE fork, no Leyline directive, no llama.cpp substrate. Their primitives are the
wrong shape — `seq_rm` frees cells (destructive), `seq_add` rotates (the thing the proposal
refuses), `kv_block_load` re-anchors (not a view). Building a view on mutation primitives
would inherit their drift and placement problems. What we take from the papers is the map
of open questions, not their code.

> 选一个能 agentic coding 的新一点的最小模型好跑 benchmark，同时节约空间留足 kv cache 来对比
> 长上下文 mask 和不 mask 效果

> hybrid模型完全可以用啊，线性state里有被masked就有呗，无所谓，反正能节省decode的kv显存就行了

Hybrid is usable and is now the primary substrate: the recurrent state's ghost is free
because the state is constant-size. Model selection and the real boundaries:
`2026-11-22-a-hybrid-state-is-a-free-ghost.md`.

> 这个框架将会不支持重新prefill … 或者说可以re prefill，但必须分段prefill,每一段的mask都是相同
> 或者交集依然很大的

Append is the only primitive; the mask log is essential durable state:
`2026-11-22-append-is-the-only-primitive.md`. Substrate, benchmark and phases:
`kv-mask-experiment-plan.md`.

## Sources

- Leyline: KV Cache Directives for Agentic Inference — arXiv:2606.01065v1, 2026-05-31,
  Bole Ma / Jan Eitzinger / Harald Köstler, FAU Erlangen.
  <https://arxiv.org/html/2606.01065v1>. SGLang 0.5.2 + patch, debug-gym 1.3.0.
  Models: DeepSeek-V2-Lite, JoyAI-LLM Flash, GLM-4.7-Flash, Moonlight-16B-A3B (all MLA MoE),
  H100/H200. Reported: replay cache-hit +14.3pp, up to 11.2 ms latency saved; a ten-line
  `truncate_older_than(n=2, max_chars=200, tools=bash)` rule vs `keep_all` lifts debug-gym
  solve rate by ~+24pp (**the fetched HTML interleaves two numbers here; verify in the PDF
  before quoting a precise figure**). Policy coverage is 8 mini_nightmare tasks × 4 seeds
  × 2 policies at **one** threshold on **one** model; the authors call the policy surface
  "the agenda" and defer it.
- EVOKE: Reversible KV-Cache Eviction as Agent Working Memory — Zenodo record 21285585,
  v4 2026-07-09. <https://zenodo.org/records/21285585>, code
  <https://github.com/Anyesh/EVOKE>, fork `Anyesh/llama.cpp`. Policy layer Apache-2.0,
  fork MIT. RTX 4070 Ti SUPER 16 GB, CUDA 13.1, FA on. Requires FA + Ampere-or-later;
  with FA off the splice is ~280× slower. `q4_0` KV collapses Qwen2.5-7B to incoherent
  output; KIVI-style asymmetric KV quantization is not in stock llama.cpp and was not run.
- KVTether — arXiv:2609.39819, <https://export.arxiv.org/pdf/2609.39819>.
- FAO (FlexAttention block-granularity caveat) — OpenReview `oSk9tP5Mgs`,
  <https://openreview.net/pdf?id=oSk9tP5Mgs>.
- CacheBlend (verified, contra the endorsement's citation) — EuroSys 2025 Best Paper,
  arXiv:2405.16444, DOI 10.1145/3790254. Its thesis is **selective KV recompute** of a
  small fraction (<15%) of tokens, i.e. position fixing alone is *not* sufficient. The
  endorsement cited it as support for "just re-rotate"; it argues the opposite.
  Implemented in LMCache as `LMCACHE_ENABLE_BLENDING`.

Operational note: `web-search`'s `fetch` returned the **full** arXiv HTML→markdown for
Leyline (151 KB) this session, not the ~8.5 KB truncation recorded in
`2026-11-17-agent-teams-papers-explained-and-compared.md`. The limit appears to be
endpoint- or page-dependent; do not assume either number.
