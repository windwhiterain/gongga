# vLLM memory adapter and offload: verified state and next step

2026-11-22, worktree `feature/kv-mask-provider`; mask-offload round at `81278dd1`, recall round at `d51d2cda30e7`.

## What is verified

### Responses memory adapter: PASS

Remote peer is `whiting_main` (`whting@192.168.1.3`), RTX 3060 Ti. `~/.agents/remotes.toml` now points to the good Linux runner `/home/whting/.gongga/payload/55fd5abc83e5a289/gongga-runner runner`; `exec(remote="whiting_main")` returned `whiting-main`, `/home/whting`, `REMOTE_OK`. Use this remote exec path for peer operations rather than assuming the old configured runner works.

Patches in local `.dev/kv-mask/` and peer `~/kv-mask/exp/`:

- `gongga_memory.py`: validates `gongga_memory/gongga_seq`, marks a render copy, requires the cleaned marked prompt to equal the original byte for byte, and maps seq spans with original HF token offsets. Unsupported/non-text items and missing masked seqs are refusals.
- `apply_server_patch.py`: installs the Responses adapter and propagates request ids through default and mamba-hybrid metadata paths.
- `apply_patch5.py`: applies per-request `kvmask_keep` at `get_paged_mask_mod`, using `q_req` (not Flex's `b`). It resolves the Responses `_0` and EngineCore random suffixes exactly and resets each reused batch row before applying a view. View files are immutable under `~/kv-mask/exp/views/`.
- View-content `cache_salt` isolates prefix namespaces. **Semantic verification runs with `--disable-prefix-caching`** because hybrid/Mamba state can otherwise contaminate results. Do not claim a canonical shared tape cache exists yet.

Observed with Qwen3.5-0.8B bf16 and vLLM 0.34.0:

- full `91.7 g`, masked `66.9 g`, restored `91.7 g`;
- concurrent full `91.7 g`, concurrent masked `66.9 g`;
- function-call drop spans `[27,74)` and `[81,86)` cover the whole tool call and tool response. The small model's function-format output is not a semantic oracle (`duct...`/`67.0 g`), so only structural mapping is asserted.
- Evidence: `.dev/verify/81278dd18f8f-vllm-memory-adapter.json`.

### Stage A live mirror: PASS

`kvmask_mirror.py` + `apply_mirror.py` register only the selected FullAttention canonical refs and perform a real post-forward D2H -> zero -> sync H2D round trip. Mamba is not copied. Group 3, block 4: **7,077,888 bytes**, `round_trip_exact=true`, `host_only_bytes=0`; every semantic check above passed in the same run. Evidence: `.dev/verify/81278dd18f8f-live-mirror.jsonl` and `...-mirror-probe.json`.

### Built-in native host offload: PASS

Small pool: `num_gpu_blocks_override=24`, `max_model_len=9472`, `max_num_seqs=1`, `max_num_batched_tokens=2048`, `gpu_memory_utilization=0.80`, `kv_offloading_size=1`, `kv_offloading_backend=native`. A→B→A was required: A→A hits GPU even with a small pool. After B evicted A, the third A produced an actual host reload:

- GPU→CPU total: **265,568,256 bytes**;
- CPU→GPU reload: **132,784,128 bytes**;
- answers before/after were identical. The text itself (`duct...`) is not used as a quality claim.
- Evidence: `.dev/verify/81278dd18f8f-native-offload.json`, plus live `/metrics` output in the mentor transcript.

## Mask-aware native offload: PASS

`gongga_offload.py` + `apply_offload_mask.py` now run on the small-pool server. Fixes made during execution:

- coverage cursor advances with `max(start, hi)`;
- the patched scheduler imports `FullAttentionSpec`;
- released bytes use the manager's per-group `layer_names` (`GroupOffloadConfig` has none);
- the probe uses one contiguous masked message and 380 lines: per-message template gaps otherwise leave every 576-token page partially visible, and the grouped 430/420-line prompts exceeded the 9472-token limit.

Observed maskedA→B→maskedA:

- `mask_offload`: pages=13, host_only_bytes=92,012,544;
- GPU→CPU total 251,412,480 bytes; CPU→GPU reload 26,615,808 bytes;
- first and second masked answers identical (`1.00`; not a semantic oracle).

Evidence: `.dev/verify/81278dd18f8f-mask-offload.json` and `.dev/verify/81278dd18f8f-mask-offload.jsonl`.

This proves sparse host reload for fully masked FullAttention pages under GPU pressure. It does not yet prove a canonical shared immutable tape or recall of masked content into a different physical block. Mamba remains on the original path.

## Canonical-tape recall: PASS

`recall_probe.py` runs one server lifecycle over seven scenarios (full0, mask1,
pressure, mask2, recall3, mask4, full5) on the small-pool server. The canonical
salt is view-independent (`gongga_memory.py` keys `cache_salt` on `upto_seq` and
the previous salt, not on `drops`), so all views of one tape share one cache
namespace; the ordinary tail lives in partial pages recomputed per view. At
nulling, `gongga_offload.py` evicts the stale block hash
(`_maybe_evict_cached_block` + `free_blocks(evict_first=True)`) so no later view
prefix-hits stale GPU content; the canonical copy lives in the host store.
`record_load` logs every host-load destination.

Observed (Qwen3.5-0.8B bf16, vLLM 0.34.0, torch 2.14.0+cu130):

- mask2 nulls 13 fully-masked FullAttention pages (group 3, indices 1..13),
  host_only_bytes=92,012,544;
- recall3 reloads every freed page at its original logical index (indices 0..13,
  14 pages), CPU→GPU 118,628,352 bytes; **12 of the 13 freed pages land in a
  different physical block** (`replaced_indices`);
- full0 == recall3 == full5 and mask1 == mask2 == mask4 (temperature 0; the text
  `duct...` is not a quality oracle);
- mask4 GPU-resident (no offload), full5 pure GPU hit (CPU→GPU delta 0), mask1
  GPU prefix hit (no events).

All nine checks pass. Evidence: `.dev/verify/d51d2cda30e7-recall.json` and
`.dev/verify/d51d2cda30e7-recall.jsonl` (raw events, sha256
`ce6d742007d1c01f81d8ed984cdf7f1b79b74a573c7ce32ed1b42989d887d46a`); the verbose
per-scenario report stays on the peer at `~/kv-mask/exp/recall_probe.json`.

### FlexAttention block-size fix (reusable)

`start_recall.sh` first omitted the flex kv block-size override and engine core
init failed: `flex_attn_kv_block_size must be a power of 2 ... got 576, None`.
With torch >= 2.9 `supports_small_blocks=True`, so FLEX_ATTENTION takes
`kv_block_size = cache_block_size`; the hybrid model's cache block size is 576
(not a power of 2). The prior mask-offload round ran with
`flex_attn_kv_block_size=128` (its events show `block_size '(16, 128)'`,
`direct_build false`). vllm exposes this field **only** through
`--attention-config '{"flex_attn_kv_block_size": 128}'` (a JSON dict built into
`AttentionConfig`); there is no standalone `--flex-attn-kv-block-size` flag
(`--attention-backend` is the only standalone one). Adding the JSON override
makes flex initialize (kv_block_size 128, direct_build false) and the probe pass.

## Harness integration (slice 3): PASS

A real mask-mode harness session now talks to the patched vLLM end to end. An
isolated temp-home test server (`.dev/kv-mask-home`, never the live home) built
from this worktree registers the peer as a provider with
`api = "openai-responses-memory-mask"`; `new_session --live_compact_mode mask`
selects it. The harness emits `gongga_memory {upto_seq, view_generation, masked}`
plus per-row `gongga_seq`, POSTs to the peer's `/v1/responses`, and gets `200 OK`
across turns. The adapter materializes the harness's view: `upto_seq` tracks the
session's growing transcript (6 → 8 → 10), `view_generation = 0`, `drops = []`,
canonical `cache_salt`; the flex hook applies it (`block_size '(16, 128)'`).
Evidence: `.dev/verify/c95aa6ba8b00-integration.json`; script
`.dev/kv-mask/integration_test.sh`.

Fixes found by running it:

- the endpoint `base_url` must include `/v1` (the harness appends `/responses`;
  vLLM serves `/v1/responses` — without `/v1` it 404s);
- the test server's supervisor respawns its worker, so cleanup must kill the
  supervisor tree (matched by the temp home in the command line), and readiness
  must wait for a command to answer, not just `server.lock` to exist;
- the home `config.toml` is read by nobody (the ship content's config wins), so
  mask mode is set per session, which overrides the home default.

Limit: the 0.8B probe model outputs `duct` and never calls `memorize`/`recall`.
The view-change cycle (agent-loop tool call → `store.memorize`/`recall` →
`view_generation` advance → masked request → vLLM masks/restores) is wired in the
harness and proven on the vLLM side by the recall experiment, but exercising it
through the harness needs an instruction-following coding model on the
mask-capable endpoint. `dispatch_tool` runs only the thin tool wrapper, not the
store commit, so `memorize` cannot be driven from the cli.

## MiniCPM5-2B trial: clean model, marker bug fixed

MiniCPM5-2B (OpenBMB, 2026-09-07, Apache-2.0) is a **dense `LlamaForCausalLM`
(2.52B, 42 layers, GQA 16:2, 131K context) — no Mamba**, so the masked-span
leakage limitation of the hybrid probe disappears: the mask mechanism applies to
every FullAttention layer. Downloaded from ModelScope (HuggingFace is unreachable
from the peer); serves cleanly on the 3060 Ti at `gpu_memory_utilization 0.85`
with the patches (flex block_size 16, no 576 hybrid issue, OffloadingConnector
active, no OOM). The harness mask-mode session reaches it (`/v1/responses`,
200 OK, views materialized). Evidence: `.dev/verify/c95aa6ba8b00-minicpm5.json`.

**Real bug found and fixed.** Turn 2+ requests 400'd with `memory marker rendering
did not preserve exactly one marker`: the `gongga_memory` adapter inserts
`__GONGGA_<nonce>_START/END__` markers and requires each to render exactly once,
but MiniCPM5's chat template transforms the assistant's 🙁 thinking content in
history, breaking the count: the template extracts reasoning via
`content.split('</think>')[0].split('<think>')[-1]`, discarding everything before
`<think>` — including the adapter's START marker at content start, so START
rendered 0 times. **The fix preserves thinking** (a hard requirement): capture the
pre-`<think>` prefix as `think_lead` and re-emit it before `<think>` in the
reasoning branch, so START survives. Thinking stays on; multi-turn mask requests
return 200 (turn 2 `bytes=14655`, no 400). Backup: `chat_template.jinja.bak`.

vLLM 0.34 quirks: the non-streaming Responses tool path 500s
(`'FunctionTool' object has no attribute 'function'`); streaming (the harness's
path) works. `--chat-template-kwargs` is not a CLI flag (per-request field).
MiniCPM5 tool calling works in streaming Responses: with `tool_choice=required`
the `minicpm5` parser yields a parsed `function_call` (`memorize`).

## Remaining work

The harness ↔ inference-service contract is proven for generation-0 views, on a
clean dense model. The open link is getting the model to **call** `memorize` in a
real harness turn: with thinking ON and no `tool_choice`, MiniCPM5 reasons about
calling memorize then emits no tool call (`view_generation` stayed 0); a direct
probe confirms thinking + `tool_choice=required` yields a parsed `function_call`. The view-change cycle is wired in the harness and proven on
the vLLM side, so the remaining link is a harness `tool_choice=required` on the
checkpoint ask, so a thinking model must memorize when asked — not a
mask-mechanism gap. The CUDA arena's `nvidia-smi` total still does not fall
(`free_blocks` changes reusable capacity, not reserved memory).

## Process state at handoff

No vLLM process or GPU compute process remains. `nvidia-smi --query-compute-apps` returned no rows. The recall server (pid 42333) was stopped by its recorded pid. No new server should be left running at turn end.
