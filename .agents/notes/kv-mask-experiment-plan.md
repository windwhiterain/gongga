# KV-mask experiment plan

Date: 2026-11-22. Session `kv-mask`. **Status: the direction is settled by user verdicts;
nothing is built, and permission to write experiment scripts has not been given.** The
model choice below follows from those verdicts rather than from a separate approval. Do not
treat any phase as started.

Supersedes the version of this note written by an earlier dream, which carried the
overturned "hybrid is disqualified" conclusion and a five-arm centerpiece.

Findings this plan rests on:

- `2026-11-22-a-kv-mask-is-a-view-not-an-edit.md` — the route distinction, the literature
  map, the EVOKE/Leyline contradiction (now secondary).
- `2026-11-22-a-hybrid-state-is-a-free-ghost.md` — hybrid is usable and preferred; the real
  boundaries; model table and VRAM estimates.
- `2026-11-22-append-is-the-only-primitive.md` — the segmented-replay contract, the mask log
  as essential state, and the retraction of the "restart loses the ghost" trade-off.

## The instructions that shape it

> 我想让你在本地引擎上验证，我正在给一个 3060Ti 8G 显存 + 32G 内存的机器装 ubuntu server，
> 装好之后你就在那个上面实验。你现在先定计划

> 我觉得他们的路线和我完全不同吧，不要再套用他们的原语了。

> 选一个能 agentic coding 的新一点的最小模型好跑 benchmark，同时节约空间留足 kv cache 来对比
> 长上下文 mask 和不 mask 效果

> hybrid模型完全可以用啊，线性state里有被masked就有呗，无所谓，反正能节省decode的kv显存就行了

> 这个框架将会不支持重新prefill … 或者说可以re prefill，但必须分段prefill,每一段的mask都是相同
> 或者交集依然很大的

So: validate on a local engine on that box; **no EVOKE fork, no Leyline directive, no
llama.cpp**; hybrid is allowed and preferred for KV economy; the point is **long-context
masked vs unmasked**, and the framework has **no dense prefill path**.

## Substrate: our own tape + FlexAttention

The primitive needs exactly one thing the papers' primitives do not offer: an **immutable,
position-addressable KV tape plus a per-request attention mask**. PyTorch's FlexAttention
expresses it literally, and it is not an eviction/compaction primitive:

```python
# tape: append only. never rotated, never freed, never moved.
tape.k[layer], tape.v[layer]

# this request: the harness's span mask, expanded to positions
def mask_mod(b, h, q_idx, kv_idx):
    return (kv_idx <= q_idx) and active[kv_idx]

block_mask = create_block_mask(mask_mod, B, H, Q_LEN, TAPE_LEN)
out = flex_attention(q, k, v, block_mask=block_mask, enable_gqa=True)
```

`create_block_mask` classifies blocks as full / partial / empty and **the kernel skips empty
blocks entirely, saving compute and HBM reads** — that is "only load the unmasked KV".
Positions are the original `position_ids` we pass, so **`preserve` is constructed, not a mode
flag**. No `seq_rm`, no `seq_add`, no host pool, no re-anchoring.

Implementation route: HF weights + our own append-only tape + our own attention path calling
`flex_attention`. First check whether transformers' `attn_implementation="flex_attention"`
can express a per-request BlockMask over a growing tape; if not, patch the attention forward
ourselves.

**On the hybrid primary, only the 8 full-attention layers are patched.** The Gated DeltaNet
layers run stock from the transformers `qwen3_5` implementation — we do not write GDN. Their
state is never masked and never changes, which is why unmask is exact there. **Qwen3 has
QK-Norm — do not drop it** on the pure-attention control.

### The contract the substrate must honor

From `2026-11-22-append-is-the-only-primitive.md`, and it is a requirement rather than an
optimization because the target hardware is sized for sparse decode:

- `append(tokens, mask)` is the only operation; rebuilding history is a **replay of appends**.
- A **segment** is the maximal run appended under one mask; segment boundaries are span
  boundaries; chunk freely inside a segment.
- The GPU holds the active working set **B**; the tape lives in host RAM/NVMe. **Offload is
  core.** B must cover the maximum active set over *all* segments, since unmask is not
  monotone.
- The **mask log is essential durable state**; KV bytes are rebuildable, the log is not.

### Two known risks, stated before building

1. **Block granularity is not bandwidth.** FAO (OpenReview `oSk9tP5Mgs`) measures that
   FlexAttention loads at block granularity, so fragmented active blocks incur redundant
   HBM transactions — whole physical blocks fetched for negligible valid context — and a
   defragmented dense layout is what turns logical sparsity into wall-clock speedup.
   Partially-visible blocks are read in full. **Therefore block-aligned vs token-granular
   masking is an experimental axis, not an implementation detail.** It is also the
   proposal's own "方案 B: block 对齐 + 边界 mask".
2. **Compilation.** `create_block_mask` compiles per `(B, H, Q_LEN, KV_LEN)`; during decode
   `KV_LEN` grows every step. Recompilation overhead must be handled (fixed max length +
   dynamic shapes, or cached block masks). This is the main time sink of P1 and is an
   engineering problem, not a scientific one.

## Models

| role | model | tape target | why |
| --- | --- | --- | --- |
| **primary** | **Qwen3.5-4B** (GDN hybrid + MoE, 8/32 full attention, 32 KiB/token) | **96K comfortable, 128K tight** | 4.5× the context of the pure-attention option on the same card; 262144 native so no YaRN; FrogNano / smeagle-4b show it drives an agent loop |
| **control** | **Qwen3-4B-Instruct-2507** (pure GQA, 144 KiB/token) | 24K–32K | every layer is maskable, so the intervention is not diluted |

**The control is an axis, not a fallback: how much does the recurrent summary cover for the
mask?** Nobody has asked this. EVOKE tested hybrid architectures for eviction/recovery, not
for a harness-declared mask.

Probe design follows from it: **exact-fact probes** (port number, identifier, path) and
**gist probes** (what the conclusion was). Prediction: on hybrid, masking preserves gist and
breaks exact facts; on pure attention both break unless a downstream row's ghost carries
them.

Geometry, VRAM estimates and the hybrid's real boundaries are in
`2026-11-22-a-hybrid-state-is-a-free-ghost.md`. Light-KV fallback if a longer tape is needed
on the control: Qwen2.5-Coder-3B-Instruct (36 KiB/token), accepting the YaRN confound.

## Centerpiece: the four-arm segmented-replay verdict

This is the experiment that decides the user's architectural claim, and it replaces the
five-arm design as the primary.

Build a tape of **N segments with heterogeneous masks**, active set held at a fixed budget
**B**. Reference = the live tape's KV₀.

| arm | how history is rebuilt | prediction |
| --- | --- | --- |
| **1** | one dense prefill of the whole token sequence, no mask | KV diverges from KV₀; peak resident grows with N |
| **2** | one prefill under the **intersection** of the historical masks | same, and ≈ no masking at all |
| **3** | one prefill under a **per-query 2D mask** (FlexAttention can express it) | **KV ≈ KV₀ mathematically, but peak resident still grows with N** |
| **4** | **segmented replay**, each segment under its own `M_i`, offloading mid-pass | KV ≈ KV₀ within numerics; peak resident ≈ B, **flat in N** |

Measurements: per-layer KV rel-L2 / cosine against KV₀; next-token logits KL; argmax
agreement; **peak resident KV bytes**; wall time. Sweep N with B fixed.

**Arms 3 vs 4 are the verdict.** If 3 is mathematically right but memory-wrong, the
constraint is proved to live in the hardware/scheduler layer, not in the mathematics — which
is the precise version of "hardware customized for decode masks cannot hold the dense KV to
prefill". If 3 also diverges, the union argument is not the whole story and the mask-history
dependence needs a closer look.

**This box can run both paths**: a 128K hybrid tape dense is ~6.3 GiB, so arms 1–3 fit on the
8 GB card alongside arm 4. The peak-resident gap is measured directly, not argued.

## Secondary experiments (demoted, still worth running)

**Five arms on one server life** — the masked-vs-unmasked quality comparison. Task suite: a
small synthetic repository, 6–10 agentic coding tasks (locate a bug across ~3 hops → edit →
run tests), each producing 20–40 tool turns of tape. Frozen probes at known spans (the
ClaimKeep method in `harness-exp-frontier-survey.md`, where compaction survival of exactly
this kind of probe is worst).

| arm | view | question |
| --- | --- | --- |
| A | whole tape, no mask | reference |
| B | hollowing: old tool-output bodies masked, head/tail anchors kept | the proposal's claim |
| C | whole spans masked, no anchors | what anchors are worth |
| D | downstream conclusions masked too | the price of erasure — **unavailable on the hybrid primary** (the recurrent state cannot be erased without recompute); run on the pure-attention control only |
| E | re-prefill the masked view (= gongga's text compaction today) | what the ghost is worth |

B vs E quantifies the ghost. Crossed with block-aligned vs token-granular masking.
Measurements: argmax / first-token agreement against A; probe recall split exact-fact vs
gist; task completion; attention mass on anchors; prefill tokens avoided; decode tok/s;
TTFT. Do not score on solve rate alone.

**The EVOKE/Leyline contradiction** (both describe `seq_rm` + `seq_add` + deferred RoPE
shift; EVOKE claims behaviour identical to reading the truncated text, Leyline §4.1
constructs a case where full-context and re-prefill disagree on the first decoded token).
Settled as a side effect of B vs E on our own substrate, without their code. Details and
citations in `2026-11-22-a-kv-mask-is-a-view-not-an-edit.md`.

**Position-gap gradient**: masked span 1K / 8K / 20K under `preserve`, quality vs gap size.
The primary mode neither paper measured. On hybrid this also exposes the layer-type
inconsistency (state sees contiguous history, attention sees holes).

## Phases and gates

| phase | work | gate |
| --- | --- | --- |
| **P0** (here, before the box) | confirm the transformers FlexAttention integration or write our own attention path; tape + mask engine skeleton; segmented replay + offload; provisioning script | skeleton runs a 3-layer toy on CPU with correct mask semantics, and a segmented replay reproduces a heterogeneous-mask tape |
| **P1** (box ready) | int4 weights + bf16 KV on sm86; measure real VRAM; fix tape length (96K/128K hybrid, 24K/32K control); drive one agentic task to a real trajectory | model clears a capability floor on the task; `nvidia-smi` numbers written to disk |
| **P2** | **the four-arm verdict**, N swept with B fixed | one-line verdict + `.dev/verify/kv-mask-*.json`; arms 3 vs 4 separated |
| **P3** | five arms × probes × two block granularities, on both models; position-gap gradient | the hybrid-vs-pure-attention contrast stated with numbers |
| **P4** | wire gongga: `live_compact` checkpoint boundaries → `span_ids`; B vs E on real benefit | only if P2/P3 give a positive signal |

P4 is where the harness payoff is: `live_compact` is already `exclude_spans` expressed as
text, the checkpoint boundary is already a span id, and the SPEC's append/cut invariant is
already the tape. Reversible folding would make "recall over cut history"
(`harness-exp-frontier-survey.md`, item 3) an **in-place unmask** instead of appending a
snippet at the end out of order. But gongga already avoids most middle edits (tail-folding
checkpoints, `reference_revision` diff replay, append-only), so the marginal win must be
measured, not assumed.

## Discipline

- Experiment code under `.dev/kv-mask/`; **not** in the repo test suite. Writing tests
  needs the user's permission and has not been given.
- Results as data: `.dev/verify/kv-mask-<engine-sha>-<area>.json` — build, command,
  scenarios, result, duration. One-line verdict, not prose.
- L3 rules from `AGENTS.md` apply to any real-process script: own temp home, self-cleanup,
  pid from `server.lock` never `$!`, readiness by dial not sleep, scrub model env vars on
  server start, `trap` kill + rm.
- Paper numbers are claims to reproduce, not conclusions.

## The box is a dependency, and it is not ready

Read `2026-11-21-install-blocked-at-installer-error.md` and
`2026-11-21-the-wire-between-the-two-machines.md` before asking the user anything about
access. Much of what this plan listed as unknown is already settled there:

- The box is the **workstation `PS2022JJWZLIXG`, remote name `lichen-vm`, wired
  192.168.1.3**, being replaced Windows 11 → Ubuntu 24.04 LTS server unattended, then handed
  to an agent over SSH. The laptop (`YanYangTheSelection3`, WLAN 192.168.2.254) is on a
  **different subnet**, which is why a remastered USB stick was chosen over a cable.
- **The install is blocked, and the cause is settled**: the stick boots and the installer
  starts, then subiquity crashes in its Mirror controller with `argument of type 'NoneType'
  is not iterable`, because the seed's `apt:` block uses the legacy mirror form with no
  `arches` key. The fixed block is in that note. It is a seed defect, not a stick defect —
  the stick's capacity/health verdict was re-measured and the earlier "fake capacity"
  finding does not reproduce.
- The workstation is **unreachable while it is in the installer**; `exec` answering "the
  peer did not say where its home is" is expected, not a fault to fix.
- The intended setup includes **full-rate FP16/BF16 tensor cores via `Zagitta/gpu-unlock`**.
  That is a dependency for any bandwidth/throughput number P2/P3 reports: measure after it
  is applied, and record whether it was.
- **Disk hazard**: the workstation's `C:` was at 0 bytes free (WSL failed to start) and is
  at ~4.73 GB free with a 140 GB `ext4.vhdx` on it. Anything staged there needs room made
  first.
- **Models physically available offline are Qwen2.5-1.5B and Qwen2.5-0.5B**, on the doomed
  disk and in a verified copy on the stick (exFAT volume `MODELS`, no partition entry, at
  byte offset 8,590,983,168). Both are far too small for agentic coding, so the primary
  model still has to be downloaded — **if the box has no network route, P1 cannot start**.
  The install note's warning stands: copy the models off the stick before anything
  reformats it.

So P0 is the only phase that can proceed now, and it proceeds here, on a toy model, without
the box.

## Still needed from the user

1. SSH to the box once the install completes: credentials and whether sudo/package install
   is allowed. Host and remote name are already known (above); the subnet gap is not yet
   resolved.
2. Whether the box reaches the internet directly (Qwen3.5-4B int4 ~3 GB + torch/triton). The
   `flm` whitelist is on the Windows machine; the Ubuntu box needs its own route. The offline
   models on the stick do not substitute.
3. Free disk (≥25 GB suggested) and the CUDA driver version.
4. **Permission to write the experiment scripts** (research code, not repo unit tests).
5. Confirmation of the primary/control pair above, and of the exact Qwen3.5-4B HF repo
   (whether there is an Instruct/Thinking split).
