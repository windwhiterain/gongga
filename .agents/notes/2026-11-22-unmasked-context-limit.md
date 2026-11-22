# Unmasked context budget for KV memory masks

2026-11-22 user direction: the context budget for the remote KV-mask model is its
unmasked context, not the full masked tape, and harness memory-block eviction must use
the provider's unmasked-context ceiling.

## Measured starting point

The peer MiniCPM5 endpoint published only `max_model_len = 55296`. An identical
34-token request returned `usage.input_tokens = 34` both with an empty masked list and
with its first input item masked. The existing harness therefore measured full-tape
input, and its only autonomous visibility limit was the configured unmasked-block
count.

## Contract

`input_tokens` remains the complete append-only tape. A memory-mask provider also
reports `gongga_unmasked_input_tokens` and `gongga_memory_tokens`; the latter records
the provider-tokenized size of each masked memory block. A block is masked when first
created, so its size is measured before a later recall can make it visible.

The roster's `context` remains the full-tape input+output ceiling. Its optional
`unmasked_context` is the visible input+output ceiling; mask mode falls back to
`context` when no separate value is stated. Context notices, live-compact growth,
output-space calculation and memory eviction use the unmasked values in mask mode.
Cache hit ratios continue to divide by full-tape `input_tokens`.

The existing FIFO block-count limit still applies. The unmasked token limit is the
session's configured percentage of the provider's unmasked window. `memory_state`
keeps a projected visible size for the current view, calibrated by provider usage.
Recall adds measured block sizes and masks the FIFO head until both limits hold.
Memorizing a previously unmeasured block makes the projection unknown until the next
response; no character-to-token estimate is invented. A model change invalidates
provider-specific block measurements. The provider independently refuses a request
whose actual unmasked input plus requested output exceeds its declared ceiling.

The peer source publishes `max_unmasked_model_len`, validates
`KVMASK_UNMASKED_CONTEXT`, and reports both usage extensions. Its serving configuration
uses the model's native 131072-position tape ceiling, 55296 visible tokens, 2.25 GiB
GPU KV and 4 GiB native host offload. vLLM's startup planner also uses the visible
ceiling for its GPU-capacity check; without that patch it refused 131072 positions for
requiring 5.25 GiB against the available 2.25 GiB.

The user explicitly stopped the unused 8311 service for verification. The replacement
records its pid in `exp/minicpm.pid` and is stopped when this session's verification
ends. `/v1/models` returns `max_model_len=131072` and
`max_unmasked_model_len=55296`. The original 34-token pair now measures:

- full view: `input_tokens=34`, `gongga_unmasked_input_tokens=34`;
- first item masked: `input_tokens=34`, `gongga_unmasked_input_tokens=22`,
  `gongga_memory_tokens=[{memory_id: mem:1:1, tokens: 12}]`.

With `max_output_tokens=55270`, the full view is refused with 400 because 34+55270
exceeds 55296; the masked view succeeds because 22+55270 fits. This is a real request
against the patched vLLM process, not a serialization-only check.

## Reasoning and tool parsing

The `probe` session showed thinking in `output` with `reasoning_items=0`. Enabling
`--reasoning-parser minicpmv` and restarting the verification server fixed the streaming
contract. A real `tool_choice=required` request produced 105 `reasoning_text.delta`
events, zero `output_text.delta`, and a completed `function_call` named `memorize` with
`{"summary":"provider parser probe"}`; there were no error events. The serving script
keeps both `--reasoning-parser minicpmv` and `--tool-call-parser minicpm5`.

No test files are added, per the repository rule.

## Harness verification and limits

The rebuilt harness ran against the peer in an isolated home. A generation-0 request
stored `context_tokens=3530` and `tape_tokens=3530`. Parsed reasoning history initially
failed because the adapter rejected `reasoning_text`; allowing that content type fixed
the following request. The small model did not voluntarily call `memorize` (high
reasoning answered without the call; `none` produced empty responses), so no voluntary
model-driven memory commit is claimed. A direct provider request with
`tool_choice=required` did produce the call.

For the eviction path itself, two fixture blocks were inserted into the stopped test
home, each with 1000 measured tokens and a projected visible total of 4000. Real
`set_session` calls then proved:

- at 6% (3317 tokens), exactly `mem:1:1` was masked, leaving one block and 3000
  projected tokens;
- at 1% (552 tokens), `mem:1:2` was masked too. With no raw block left, config and
  request enforcement return normally instead of looking for a nonexistent FIFO head
  or advancing `view_generation` without a view change.

After mapping the fixtures to real seq spans, a provider request stored
`context_tokens=4472` and `tape_tokens=4723`, exactly 226+25 masked tokens apart. A
later request stored 8096 visible versus 8347 tape tokens. Config and request
enforcement both left generation 4 unchanged with no evictable block.

A 60023-token fully visible request was refused as intended. Masked 60023-token and
56223-token tapes did not finish within 180 and 300 seconds respectively; long
FlexAttention prefill remains a performance limitation, not a demonstrated semantic
pass. These attempts are not counted as successful long-tape verification.

`mbx build` passed after the final generation guard. clean-disk ran after the mbx low
space report: exit 0, 415547392 bytes actually freed, `recycle_left=0`.
