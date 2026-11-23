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

## Status
- worktree `.worktrees/cmd-checkpoints`, branch `feature/cmd-checkpoints`, base b26e2073.