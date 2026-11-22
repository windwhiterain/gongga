# A hybrid state is a free ghost

Date: 2026-11-22. Session `kv-mask`. **This note supersedes and replaces
`2026-11-22-a-hybrid-layer-cannot-mask-its-middle.md`, which is deleted.** That note's
thesis was overturned by the user; its architecture facts were fetched and are kept here.
VRAM figures are estimates, not measurements. Capability scores from third-party blogs are
untrusted.

## The verdict that overturned it

> hybrid模型完全可以用啊，线性state里有被masked就有呗，无所谓，反正能节省decode的kv显存就行了

The deleted note argued that a middle mask is *undefined* on a recurrent layer, therefore
hybrid models are disqualified. That conflated two different things:

- **Erasure** — making the model behave as if it never read the span. Undefined on a
  recurrent layer without recomputing the state. True, and irrelevant here.
- **Masking** — not loading the span's per-position KV at attention time. Perfectly
  defined on the full-attention layers, and the recurrent layers were never the point.

The proposal asks for the second. It also missed the decisive fact: **a recurrent state is
constant-size per sequence. It does not grow with context.** So the ghost it carries is
**free** — it must be kept anyway and costs no per-token budget. "24 of 32 layers still see
the span" is not a loss when those layers' history costs nothing to keep.

## Why hybrid is not merely usable but the better substrate

**1. The architecture already implements the proposal's fold semantics.** Gated DeltaNet
layers keep a *learned, constant-size gist* of everything; the full-attention layers keep
*retrievable detail*. Masking the detail and leaving the gist is exactly "hollowing" —
keep the anchors, empty the payload — except the anchor is learned and free instead of
hand-picked head/tail tokens.

**2. KV economy is the practical win, and it is large.** Only the full-attention layers
have per-token KV:

| model | full-attention layers | KV geometry | KV per token (bf16) |
| --- | --- | --- | --- |
| **Qwen3.5-4B** | 8 of 32 (`full_attention_interval: 4`) | 4 KV heads × head_dim 256 | **32 KiB** |
| Qwen3-4B-Instruct-2507 | 36 of 36 | 8 KV heads × head_dim 128 | **144 KiB** |

`KV/token = 2 (K,V) × full_attention_layers × kv_heads × head_dim × bytes`. **4.5× more
context on the same card**, with 262144 native positions so no YaRN confound. This is what
"节约空间留足 kv cache" buys.

## The real boundaries (these replace the old "disqualification")

1. **The mask acts only on the full-attention layers, so the intervention is diluted.**
   What is measured is "remove retrievable detail while a learned summary keeps
   everything". Probes must therefore split in two: **exact-fact probes** (a port number,
   an identifier, a path) and **gist probes** (what the conclusion was). Falsifiable
   prediction: on hybrid, masking preserves gist probes and breaks exact-fact probes; on
   pure attention both break unless a downstream row's ghost carries them. That contrast
   is the reason to keep a pure-attention control model at all.
2. **True erasure is impossible on hybrid** without recomputing the state from the mask
   point. Leyline's `forget` mode has no hybrid equivalent. Only `amortize` is available.
   The proposal does not need `forget`; a redaction/retention product would.
3. **A new OOD risk, untested:** the recurrent state sees a *contiguous, complete* history
   while the attention layers see a *holed* one. That layer-type inconsistency does not
   exist in a pure-attention model and the model was never trained on it. Whether it is
   benign is an empirical question, and it is the proposal's own open corner on hybrid.
4. **Reversibility is perfect — better than on pure attention.** The state is never
   touched, so unmask restores the attention KV and the view is exactly the original.

## Architecture facts (fetched this session, kept from the deleted note)

**Qwen3.5-4B is a GDN hybrid + sparse MoE.** From `thu-pacman/chitu`,
`chitu/config/models/Qwen3.5-4B.yaml` (2026-04-27) and HF mirrors:

- `type: hf-qwen3-5`, dim 2560, **n_layers 32**, n_heads 16, **n_kv_heads 4**,
  **head_dim 256**, vocab 248320, intermediate 9216, `use_qk_norm: True`
- `linear_conv_kernel_dim: 4`, `linear_head_dim: 128`, `linear_n_qk_heads: 16`,
  `linear_n_v_heads: 32`, **`full_attention_interval: 4`**
- `max_position_embeddings: 262144`, `rope_theta: 1e7`, `mrope_interleaved`,
  `mrope_section`, `partial_rotary_factor`, `mamba_ssm_dtype: float32`
- model card: "Efficient Hybrid Architecture: Gated Delta Networks combined with sparse
  Mixture-of-Experts"; causal LM **with a vision encoder**

The family has 0.8B / 2B / 4B / 9B siblings, all the same GDN hybrid linear-attention
family (`john-rocky/coreai-model-zoo`, 2026-11-14). EVOKE independently describes
Qwen3.5-9B as "hybrid Mamba/Attention + mrope, thinking-mode". `qwen3_5` support exists in
transformers and vLLM (`vllm/transformers_utils/configs/qwen3_5.py`, `Qwen3_5TextConfig`,
`model_type = "qwen3_5_text"`).

**The agentic fine-tunes on it are now an argument for it, not against:**

- **FrogNano** (arXiv:2609.07925, v3 2026-10-05): 4B coding agent, RL over ~1500 synthetic
  SWE environments, online task synthesis at the learnability frontier, 131K context.
  Built on Qwen3.5-4B.
- **smeagle-4b** (`Hob-forge/smeagle-4b`, HF, 2026-08-22): agentic terminal/SWE helper,
  "fits in a few GB". `base_model: Qwen/Qwen3.5-4B-Base`.

Together they are evidence the base can drive an agent loop, which is what the benchmark
needs. Whether either publishes weights is not checked.

## Candidates

| model | architecture | KV/token (bf16) | native ctx | role |
| --- | --- | --- | --- | --- |
| **Qwen3.5-4B** | GDN hybrid + MoE, 8/32 full attention | **32 KiB** | 262144 | **primary** |
| **Qwen3-4B-Instruct-2507** | pure GQA, `model_type: qwen3`, 36/8/128 | 144 KiB | 262144 | **pure-attention control** |
| Qwen2.5-Coder-3B-Instruct | pure GQA, 36/**2**/128 | 36 KiB | 32768 (YaRN 131072) | light-KV fallback; geometry from knowledge, **not fetched** |
| Nanbeige4.2-3B | **looped** Llama, 22 blocks × `num_loops=2`, **44 KV slots** | — | 256K | pure attention but exotic; not first |
| Gemma 4 E4B / 12B | SWA + global | light | — | E4B too weak at agentic coding; 12B weights too big for 8 GB |
| Qwen3-Coder-30B-A3B | MoE | — | — | weights too big for 8 GB |

**Nanbeige warning worth keeping**: a 22-slot KV cache **does not crash — it silently emits
wrong output**; the slot index is `layer_idx + loop_idx * num_hidden_layers`. It needs
`NanbeigeConfig` / `NanbeigeForCausalLM` and carries hyper-connection / `mhc` /
`depth_attention` options. Its eye-catching scores (SWE-Bench Verified 63.6, Terminal-Bench
2.0 44.1, beating Qwen3.5-9B) come from one third-party blog (`webbrain-one/webbrain`,
2026-11-16) and are **unverified and implausibly high for the size**. Do not plan around
them.

**Qwen3-4B-Instruct-2507** geometry confirmed from three independent config uploads
(`Qwen/Qwen3-4B-Instruct-2507-FP8`, `zhengchy/…-ov`, `Translsis/…`): 36 layers, 32 query /
8 KV heads, head_dim 128, hidden 2560, intermediate 9728, vocab 151936, RoPE theta
5,000,000, 262144 context, tied embeddings, non-thinking. Terminus-4B (arXiv:2605.03195,
SFT+RL for terminal execution, claims 30% context reduction) is further evidence the base
drives an agent loop.

## VRAM on the target box (RTX 3060 Ti 8 GB, 32 GB RAM, Ubuntu Server)

sm86 (Ampere); FlexAttention / Triton run on it. FA4's block sparsity is newer and likely
Blackwell-targeted — do not plan on it here.

Estimate, int4 weights + bf16 KV, activations + CUDA ctx ~0.8 GiB:

| model | tape | KV | weights | total |
| --- | --- | --- | --- | --- |
| Qwen3.5-4B | 96K | 3.0 GiB | ~2.3 GiB | **~6.1 GiB** ✓ |
| Qwen3.5-4B | 128K | 4.0 GiB | ~2.3 GiB | **~7.1 GiB** tight |
| Qwen3-4B-Instruct-2507 | 24K | 3.4 GiB | ~2.3 GiB | ~6.5 GiB ✓ |
| Qwen3-4B-Instruct-2507 | 32K | 4.5 GiB | ~2.3 GiB | ~7.6 GiB tight |

Assumptions to verify with `nvidia-smi`: `lm_head` only on the last position (a full
2048×151936 bf16 logits tensor is ~622 MB alone), chunked prefill, vision tower not loaded.

**The budget changes shape under the segmented-replay contract**
(`2026-11-22-append-is-the-only-primitive.md`): the GPU holds the active working set **B**,
the whole tape lives in host RAM/NVMe. So VRAM is sized by B, not by tape length N, and
32 GB of host RAM holds a 128K hybrid tape (4 GiB) with room to spare.

**A lucky property of this box: both arms fit.** A 128K hybrid tape dense is ~6.3 GiB, so
the *dense* prefill arm and the *segmented replay + offload* arm can both run on the same
8 GB card. The peak-resident difference can be measured directly instead of argued.

Do not quantize KV to int8 to buy length in round one: FlexAttention needs float K/V, so
int8 KV needs a custom dequant-in-kernel path (a dequant pass materializing bf16 defeats
the saving). Weight quantization is a shared confound across arms and only lowers the
absolute capability floor.

## Implementation notes for the hybrid

- **GDN layers run stock** from the transformers `qwen3_5` implementation. We do not write
  Gated DeltaNet. Only the 8 full-attention layers' attention path is replaced with
  `flex_attention`.
- MoE experts under int4 weight-only need care (bitsandbytes / torchao).
- `mrope_interleaved` + `mrope_section` + `partial_rotary_factor`: confirm text-only input
  degenerates to 1D positions before trusting any position-gap result.
- Do not load the vision tower.
- The recurrent state is fp32 (`mamba_ssm_dtype`); small and constant, but do not cast it.
- Not settled: the exact HF repo name and whether there is an Instruct/Thinking split;
  read `config.json` on the box before relying on any number above.

## See also

- `2026-11-22-a-kv-mask-is-a-view-not-an-edit.md` — view vs edit, the literature map.
- `2026-11-22-append-is-the-only-primitive.md` — the segmented-replay contract that makes
  the GPU-holds-B budget above a requirement rather than an optimization.
- `kv-mask-experiment-plan.md` — substrate, benchmark, phases.
