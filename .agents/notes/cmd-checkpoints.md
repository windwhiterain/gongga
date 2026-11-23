# Checkpoint commands: server cmds, agnostic agent loop

User directives (2026-11-23):
1. `live_compact` / `memorize` / `recall` must NOT be in the model-facing injected tool
   set; the prompt tells the agent to call them through `gongga_cli`.
2. They must be first-class **server cmds**; a tool call is only one invocation surface.
3. The **agent loop cares nothing** about which tool is called — the command itself
   triggers the checkpoint machinery; turn-end / result-row facts ride generic signals.

## Design
- `server_catalog` + dispatch arms for the three; handlers run the body (meta embed /
  validation) AND commit (`append_live_compact_result` / `store.memorize` / `store.recall`),
  write + announce their own `tool_result` row, move the mode guards here.
- `call_id` rides `Origin::Tool { session, call_id }`; `CmdLayer::run(..., call_id)`;
  `run_command` passes `ctx.call_id`. Client/subscription origin: `None`.
- Commit reads the stored `tool_call` row through the `gongga_cli` envelope
  (`arguments.cmd.name` / `.args`), so the loop stores the call agnostically.
- Generic outcome signals: `ToolOutcome { end_turn, result_seq }`, populated by
  `run_command` from `Outcome.data`. Loop: `result_seq` Some -> command stored the result
  row (skip append, read its output); `end_turn` -> `CallEnd::EndsTurn`. No tool names.
- Tool rows -> un-offered forwarders (`offered = false`); remove special `Cmd` structs.
- config: `offered=false`, rewrite notices + standing prompt to `gongga_cli(...)`.
- Operator/client invocation has no model call anchor -> refuse clearly.

## Status — DONE (merged + published 2026-11-23)
- Squash-merged to main as `b4355069`; rebased onto main `67705d38` first (main's
  `f9660971` moved `live_compact`/`prefix_cache_plan` into `model_config`; the flat
  handler names are unchanged, so `tuning.live_compact_mode` still reads them).
- Content published: ship `81bcd5a` on main (`7c0c1f5..81bcd5a`), `offered=false` on the
  three rows + notices rewritten to `gongga_cli`. `scripts/ship.sh --check` exit 0.
- Gates after rebase: `mbx build` 0, `mbx fmt --check` 0, `test-layout-check` 0,
  server lib 40/40. comments-check: every file this change touches is clean; the only
  violations (`net.rs`, `remote/runner.rs`, `tools/detached.rs`) are byte-identical to
  main — pre-existing baseline, not introduced here.
- L3: `scripts/checkpoint_selftest.sh` PASS — model offered `gongga_cli` but not the
  three commands; `gongga_cli cmd.name=live_compact` commits a checkpoint, injects
  `next` (kind=inject), writes its own `tool_result` row keyed by the call id.
  Mask-mode L3 not feasible with the shared fake (a mask turn bails when the provider
  omits `gongga_unmasked_input_tokens`); memorize/recall share the verified plumbing
  and are covered by the lib tests. Data: `.dev/verify/9704113c-checkpoints.json`.