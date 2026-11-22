# Mask-aware prefix cache + compiled serving (MiniCPM5-2B)

2026-11-22, worktree `feature/kv-mask-provider` (harness HEAD `7151ed95`, unchanged this
round; every change is peer-side serving). User direction: (1) drop `--enforce-eager`,
(2) prefix cache must be mask-aware — "每个token的前缀是这个token本身加上他的mask".
Evidence: `.worktrees/kv-mask-provider/.dev/verify/7151ed95-mask-hash.json`.

## The hash design (replaces cache_salt)

`apply_mask_hash.py` patches `generate_block_hash_extra_keys` in
`vllm/v1/core/kv_cache_utils.py` (backup `.premhash.bak`): for a `kvmask-` request it
loads the immutable view file (id = leading `kvmask-<32 hex>`, cached per view) and adds
`("kvmask", clipped_drop_spans)` to a block's extra keys **iff the block intersects a
masked span**. `hash_block_tokens` already chains the parent hash, so:

- a visibility change forks the chain at the first affected block;
- every fully-visible block keys exactly like an ordinary request → all views of one
  canonical tape share one namespace; a grown tape prefix-hits the previous turn;
- recall to the full view hits the original blocks directly (no reload needed when the
  nulling optimization is off).

The per-turn `cache_salt` injection in `gongga_memory.py` is **removed** (backup
`.salt.bak`): salt = hash(upto_seq, drops, previous_salt) changed every turn and every
view, which is why the prefix hit rate was 0%. The salt is still computed into the view
JSON as provenance; nothing injects it. Frontend hashes are inactive here
(`EngineCoreRequest.block_hashes` stays None), so the engine hasher is the single site.

**Pitfall:** `exp/gongga_memory.py` is a source copy; `apply_server_patch.py` installs it
to `vllm/entrypoints/openai/responses/gongga_memory.py`. Edit exp/ then sync the
installed copy and clear its pyc, or the running server keeps the old behavior.

## Serving config that works on the 3060 Ti (start_minicpm_fixed.sh)

- no `--enforce-eager`; `--compilation-config '{"cudagraph_mode": "FULL",
  "cudagraph_capture_sizes": [1]}'` (decode-only graphs; max_num_seqs=1).
- `--gpu-memory-utilization 0.95 --kv-cache-memory-bytes 1395864371` (1.3 GiB).
  The extensible-cache planner otherwise withholds KV (0.98 GiB < the 1.15 GiB that
  28608 needs) **regardless of utilization**; the explicit override is the knob.
- FLEX_ATTENTION with the **dense default kv_block_size 16 — no 128 override**:
  cache-16/flex-128 corrupts memory on the dense model (masked view finishes, the next
  request dies with CUDA_ERROR_ILLEGAL_ADDRESS in build_block_mask). The hybrid proof
  (cache 576 / flex 128) does not transfer.
- native OffloadingConnector (`--kv-offloading-size 1`); the mask-nulling patch
  (`gongga_offload`) is **disabled** on dense (restored from `.kvmask-orig`, patched
  copies kept as `.nulling.bak`). With mask-aware hashes native LRU offload is correct
  by construction; nulling is a capacity optimization to verify separately.

## Measured (20,922-token prompt, temp 0)

| | eager + salt (before) | compiled + mask hash (after) |
|---|---|---|
| cold prefill | 15.0 s (~1400 t/s) | 6.9 s (~3030 t/s) |
| warm full hit | impossible (salt) | **TTFT 0.11 s, 20864 cached** |
| decode | ~17 t/s | ~27 t/s |

Mask semantics: `mask_hash_probe.py` — fullA→fullB grown-tape hit 768; maskedC forks
(drops [[25,756]], cached 0); fullD recall hits 768 and answers identically (temp 0);
late-fork diagnostic shares the 320-token visible prefix; tail-only mask hits all 320.
Thinking stays on (answers carry the model's think span; the think_lead template fix
from the MiniCPM5 round is untouched).

Observed boundary: a fork within the first ~2 blocks reports cached 0 even for the
unkeyed leading blocks (drops at token 44 → expected 32, got 0); late forks share
correctly. Real harness tapes fork after the ~18k-token system head, so this is noted,
not chased. Suspect a minimum-hit granularity in the connector/coordinator lookup.

## Process state

Backend left running by user request: peer `whiting_main` 192.168.1.3:8311, pid 48020,
GPU 6705 MiB, zero ERROR lines. Stop only via `exp/stop_minicpm.sh` when asked.

## KV ceiling on the 8 GB card (measured)

43,008 bytes/token (42 layers × 2 KV heads × 128 dim × 2 × bf16). Non-KV footprint
5374 MiB (weights 4804 + activation/graph workspace), non-torch reserve 512 MiB
(`KV_CACHE_NON_TORCH_RESERVE_BYTES`, CUDA = 1<<29). Ceiling: **2.25 GiB KV ≈ 56k
tokens**; served at `max_model_len 55296` with `--kv-cache-memory-bytes 2415919104`.
Proven with a 43,722-token prompt (answer correct, GPU peak 7677/8192 MiB, 0 errors).
Prefill degrades with length (flex block-mask build is quadratic): ~3030 t/s at 21k →
~1650 t/s at 44k; incremental turns prefix-hit and stay fast. Beyond this: int4/GPTQ
weights free ~3 GiB → ~120k tokens (native ceiling 131072). Roster updated to 55296 on
the community checkout's local branch (`a623ac7`), hot-reloaded.

## Block size: 16 measured faster than 128 (kept 16)

`--block-size 128` (cache/flex/hash/offload all consistent — the earlier crash was the
cache16/flex128 *mismatch*) passes all mask checks but prefills 15–20% slower: 21k cold
8.3 s vs 6.9 s; 44k cold 30–34 s vs 26.5 s (steady-state repeats, not compile penalty).
On torch 2.14 / sm_86 the q16/kv16 small-block flex path is the tuned one; `direct_build`
keeps block-mask construction cheap, so kernel execution dominates and prefers 16.
Fragmentation also favors 16: ≤0.15% boundary waste per 20k masked span vs ~1.3% at 128;
a ~128-token summary artifact is 8 fully-visible blocks (the index, by design).

## Remaining

1. `tool_choice=required` on the memorize checkpoint ask (harness `llm.rs`), so a
   thinking model must memorize → view-change cycle end to end.
2. Dense mask-nulling offload verification (currently disabled).
3. Flex kv granularity on dense (128 corrupts; 16 caps cold prefill ~3000 t/s).
4. int4/GPTQ MiniCPM5 for a larger window on 8 GB.