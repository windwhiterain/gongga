# Prompt is harness configuration

## User decisions

- Remove the mandatory `self-control` read from gongga's default system instructions.
- Let the benchmark inject a different initial system-prompt overlay for each harness.
- For the next quick comparison, inject a request to read the `subagent` skill into gongga and leave qwen at its own defaults.
- Treat prompt overlays as harness configuration and record them in the run report; do not require a shared prose baseline.

## Implementation

The benchmark passes an overlay to qwen's native `--append-system-prompt`. For gongga it appends the same overlay to the instructions template before the server starts, so the first stored prompt already contains it.
