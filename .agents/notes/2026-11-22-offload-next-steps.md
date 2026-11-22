# Offload: the half of the design that is still open, and where to start

2026-11-22, written immediately before a handoff so the next context does not re-derive it.
Everything already measured lives in `.dev/verify/2ab272406d25-kv-mask-*.json`.

## What is closed (do not re-measure)

- vLLM 0.34.0 + FLEX_ATTENTION runs Qwen3.5-0.8B bf16 on the 3060 Ti. Working invocation and the
  two config traps (`flex_attn_kv_block_size=128`, `enable_extensible_kv_cache=False`) are in
  `2ab272406d25-kv-mask-engine-survey.json`.
- The declared view is correct, reversible, and recompile-free: `2ab272406d25-kv-mask-flex-declared-view.json`,
  `2ab272406d25-kv-mask-bandwidth-null.json` (its first section, the stable-mask result).
- Block pruning happens through the mask_mod, not the hint: `2ab272406d25-kv-mask-pruning-confirmed.json`.
- Current patch on the peer is **v4**, applied to
  `~/kv-mask/.venv-vllm/lib/python3.12/site-packages/vllm/v1/attention/backends/flex_attention.py`,
  pristine copy beside it as `.kvmask-orig`. Patch scripts and drivers: local `.dev/kv-mask/`,
  peer `~/kv-mask/exp/`. `apply_patch4.py` restores pristine and applies A+B+C; v1/v2/v3 are superseded.

## The open half: peak resident KV

A mask removes blocks from the attention kernel's work but **not from device memory** (measured
long ago with raw FlexAttention: masked and full peak identical, 128.4 MiB). Only offload turns
the bandwidth saving into a memory saving, and offload is also what makes arm 4 of the verdict
(segmented replay with mid-pass offload) differ from arm 3 (one 2D-masked prefill).

## The invariant that decides the implementation

**The tape is immutable, so an offloaded block must not be freed for reuse.** If a block is
returned to vLLM's free pool, another request can take it and the tape is destroyed; the view
would stop being reversible, and reversibility is already a measured property I must not lose
(`restored` arm). So the shape has to be: blocks stay owned by the request, their **contents**
move to pinned host memory while masked, and they are reloaded **at the same logical positions**
when a later mask unmasks them. "Resident set" and "tape" are different sizes by construction.

Watch the second half of that: `get_paged_mask_mod` treats a block whose
`physical_to_logical` entry is negative as invalid. That is convenient while a block is masked,
but on unmask the mapping and the contents must both come back, or the tape silently loses
tokens and the failure looks like a model-quality problem instead of a bookkeeping one.

## Where the machinery is

- `vllm/v1/kv_offload/cpu/manager.py:53 CPUOffloadingManager`: `prepare_load` / `prepare_store` /
  `complete_load` / `complete_store`; `cpu/swap_blocks_triton.py:49 swap_blocks_batch` is the
  actual movement primitive.
- `vllm/v1/kv_offload/config.py`: `OffloadingGroupConfig.sliding_window_size_in_chunks`.
  The manager is organised around **hash-keyed prefix chunks** for cross-request reuse, which is
  not my shape; expect to write a policy keyed on declared spans (`cpu/policies/`) rather than
  to configure the existing one.
- The alternative is to drive `swap_blocks_batch` directly from the existing kv-mask patch and
  keep a pinned host pool myself. Fewer of vLLM's assumptions to satisfy, more of my own
  bookkeeping. Choose after reading how the scheduler owns blocks; do not choose by taste.

## Measurement that settles it

Same N-span / fixed-budget-B tape as the planned verdict. Report **peak resident KV bytes**
against mask density, with correctness checked against the live-tape KV0 reference
(per-layer KV rel-L2, next-token logits KL, argmax agreement). If peak resident bytes do not
fall while outputs stay equal to the reference, the offload is not real. Then arm 3 vs arm 4:
arm 3 is mathematically right but holds the whole tape resident; arm 4 must hold only B.

## Housekeeping

- `kv-mask/remote-fix` subagent is fixing the remote payload/skill bugs; its turn_end messages
  this session. Do not duplicate its work.
- Still undecided with the user: pointing `whiting_main.runner` at the good Linux payload
  (`payload/55fd5abc83e5a289/gongga-runner runner`) and getting the server to re-read
  `remotes.toml`. Diagnosis in `.agents/notes/2026-11-22-remote-whiting-main-three-faults.md`.
- The 4B model is deferred to offline AWQ/GPTQ quantisation (user chose the small-model route
  for mechanism work). No 4-bit on-load path exists in vLLM 0.34.0 for this card.
