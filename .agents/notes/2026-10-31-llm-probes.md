# Structured LLM timing probes

## User request

Add enough timing and token probes that every part of a gongga trajectory can be analysed, especially the unresolved quick3 split between provider generation and local stream consumption.

## Design

- Aggregate deltas in memory and write one `llm_attempt` event per HTTP attempt; do not add per-delta SQLite writes to the measured path.
- Keep raw provider `usage` JSON so every token detail the provider supplies survives, including optional reasoning-token fields.
- Measure request-head construction and transcript opening separately from network time.
- Record first/last reasoning, output, and tool-argument delta times, raw chunks/bytes, parsed event kinds, and local parse/handle microseconds.
- Add one `step_timing` row per step and one `tool_timing` row per executed call, keyed by turn and step.
- `scripts/timing_report.py` reads the events without contacting a provider.

The attempt's `process_total_us` includes JSON parsing, event handling, and sending into the bounded LLM event channel. Comparing it with attempt wall time distinguishes local parse/backpressure work from time spent waiting for response bytes. Tool execution has its own rows because tools run while the response is still streaming.

## Verification

- 26 llm tests and 6 agent tests pass.
- The full fake-provider replay audit passes four requests, a tool call, prompt edits, handover and payload replay.
- A preserved short turn produces exactly one `turn_end`, two `step_timing`, two `llm_attempt`, one `tool_timing`, and two legacy usage rows. `timing_report.py` renders all of them.
- `[DONE]` is classified as an SSE sentinel rather than a parse error.
- The replay helpers were updated for the current catalog variable, reader payload, process-tree cleanup and chunked request bodies.
