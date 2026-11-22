# Engine choice for kv-mask: vLLM + FlexAttention, not SGLang

2026-11-22. Substrate survey for the user's design (immutable KV tape + per-request
declarative mask, positions unchanged, masked KV offloaded to host RAM).
Evidence and every file:line citation: `.dev/verify/2ab272406d25-kv-mask-engine-survey.json`.
Source trees read on the peer: `~/kv-mask/src/{sglang,vllm}` (wheels from the Tsinghua
mirror, `--no-deps`; GitHub is unreachable there, so this is how source is obtained).

## Decision

**vLLM 0.34.0, FLEX_ATTENTION backend.** The handoff note carried SGLang as the starting
hypothesis. The hypothesis did not survive reading the source.

## Why

The design needs three mechanisms. Count who already has each one *wired*:

| mechanism | vLLM 0.34.0 | SGLang 0.5.23 |
| --- | --- | --- |
| mask evaluated in logical index space, positions untouched | `flex_attention.py:475`, per-layer `logical_mask_mod` | `FlashAttentionAdaptor` rewrites `page_table`/`cache_seqlens` |
| fully-masked blocks never loaded (the bandwidth saving) | `BlockSparsityHint`, `flex_attention.py:338` | same page-table rewrite |
| host offload engine | `v1/kv_offload/cpu` (`prepare_load`/`prepare_store`, `swap_blocks_batch`) | `HiSparseCoordinator`, device buffer + host staging + swap-in kernels |
| **is the selector actually called?** | yes, per layer per forward | **no**: `create_sparse_coordinator` has no caller; `attention_begin` has no caller outside its own core |
| runs on sm86 | FLEX_ATTENTION is in the default sm86 priority list | fa3 asserts `major == 8 and not use_mla_backend`, so yes |

SGLang's offload engine is genuinely good — `SparseConfig(top_k, device_buffer_size,
host_to_device_ratio, swap_in_block_size)` is almost literally the user's declared budget
B plus an offload ratio, and `HiSparseMHAMainPool` already translates full→compressed→device
with a hybrid-aware host copy. But its only end-to-end driver is
`minimax_sparse_backend.py:375`. Qwen's sparse backend is Qwen4-Exp's learned indexer, not a
mask hook. So SGLang means writing a MiniMax-style driver for qwen3_5 — the hardest part —
in order to reuse its pool.

vLLM is missing the resident-subset pool instead. That is the part I have to build either way.

## Two errors corrected during this survey

- **"SGLang's fa3 needs Hopper" — wrong.** `attention_registry.py:238-243` explicitly allows
  `major == 8` for non-MLA. Do not repeat the general FA3 lore as if it were this code.
- **"SGLang's `retrieve_topk` is the override point for a declared mask" — wrong.** It is
  called only from `sparse_coordinator.py:248`, which nothing calls. The framework is shipped
  ahead of its integration. Reading the caller graph mattered more than reading the interface.

## Convergence with the earlier FlexAttention measurements

The two rules measured on this card (`6d9f27c59126-kv-mask-p1.json`) are the constraints vLLM
states for full CUDA graphs: one stable `mask_mod` (`_maybe_get_custom_mask_mod` raises if the
layers disagree), and a fixed-capacity tape whose length lives in the mask rather than the shape.
Two independent investigations landing on the same constraint is a good sign for the substrate.

## The mask injection point, exactly

`FlexAttentionMetadata.get_paged_mask_mod` (flex_attention.py:452-476) computes
`is_valid` from the physical block mapping and then calls
`self.logical_mask_mod(b, h, logical_q_idx, logical_kv_idx)` with **per-request logical
indices**. `causal_mask_mod` is just `q_idx >= kv_idx` (:320-324). So a condition on
`logical_kv_idx` ANDed into `logical_mask_mod` is precisely the user's declared view:
it removes KV from the view and cannot move a single RoPE position.

Two supported places to install it:
- `layer.logical_mask_mod` / `layer.block_sparsity_hint` on the vLLM `Attention` modules,
  read every forward at flex_attention.py:1305-1319 (eager path).
- the `logical_mask_mod` local chosen in `FlexAttentionMetadataBuilder.build` (:1080-1094).

The catch: the model lives in a **separate EngineCore process** (observed MainProcess vs
EngineCore pids), so setting attributes from the driver script does nothing. Injection must
travel through either a PYTHONPATH `sitecustomize` with a lazy import hook, or a patch to the
installed file. Direct patch of `site-packages/.../flex_attention.py` is the simpler of the
two and is what "魔改" asks for; keep the anchored patch script so it can be re-applied or
reverted. Note the ordering trap: when full cudagraphs are enabled, `build` overwrites
`logical_mask_mod` with `custom_logical_mask_mod` *after* the default is chosen, so a patch
placed at the default assignment only takes effect in eager mode.

## Reference-comparison subtlety (matters for the verdict experiment)

The obvious independent reference — HF transformers with a 2D `attention_mask` zeroed over
the dropped ranges — is **not clean**: a 2D mask is padding semantics and may also perturb the
GDN/linear-attention layers, whereas the design's mask touches full-attention layers only.
If HF lets those zeros into the recurrence, the recurrent state diverges and a mismatch would
be an artifact of the reference, not of the view. The reference must be a **4D additive mask
consumed by the full-attention layers only**, positions left at 0..N-1. This is the same
boundary as the user's "mask 只作用在全注意力层" rule, arriving from the other side.

The two numerical facts worth establishing first, in order: a dropped range entirely beyond
the sequence end must reproduce the unmasked run bit-for-bit (the hook perturbs nothing it
should not), and only then equivalence against the 4D-mask HF reference.

## Next

Prove, in order, cheapest first: FLEX_ATTENTION loads qwen3_5 on sm86; a per-request
`logical_mask_mod` + `block_sparsity_hint` reproduces dense logits while peak resident KV drops;
then the four-arm verdict (dense prefill / intersection mask / per-query 2D mask / segmented
replay with mid-pass offload). Arm 3 vs arm 4 is still the user's whole argument.
