# cmd refactor per latest SPEC (e134ddd)

## Goal
SPEC `## Cmd` now spells positional syntax (`set_session <session> <model> […]`,
`send_message <session> <message>`, `subscribe <subscription> <pattern> [--bash] [--python]`,
`trigger_event <event>`, `del_subscribe <subscription>`, `get_messages <session> [range]`,
client `set_sticky <type> <value>`, `set_render_session <session> true|false`, `serve <port>`)
while the crate line requires: "cmd fully documented and diagnosed, transparent server cmd
wrapper, do not know server cmd". Completion must stay powerful.

## Design (decisions)
- Wire `Args = BTreeMap<String,String>` unchanged. Positional binding happens in the
  cmd layer from the *published* spec's arg order — cli keeps no command knowledge.
- `ArgSpec.positional: bool` (serde default). Declaration order = binding order.
- Explicit tokens bind left-to-right from slot 0; `Sticky::fill` afterwards fills only
  still-empty required/sticky args (typed beats sticky, same as the old flag rule).
  Pure-text expansion (`Client::message`) inserts the sticky session explicitly:
  `send_message <session> <text>`; refuses clearly when no sticky session exists.
- `--name value` / `--name=value` stay for flag args; a `--token` that names no flag is
  a diagnosed typo. Extra bare token → "takes <signature>" error. Duplicate arg → error.
- User clarified (2nd message): **cmd is the source of truth; a tool is a tunnel**
  letting agents pass structured args straight into a cmd (no escaping); registration
  becomes dynamic later. So: publish cmd wrappers for every tool (specs generated
  from the registry's schemas, `<session>` first positional), one shared
  "run tool for session" path used by both the cmd dispatch and subscription actions.
- subscribe: `--bash <name> --cmd <cmd>` / `--python <name> --code <code>` — the name
  is the SESSION's named interpreter (key {session, name}, the same shells/REPLs the
  agent's tools use). Columns: name, pattern, persistent, bash, cmd, python, code.
  On match: session notice (existing) + action via the shared runner; outcome logged
  as a `subscription` event row.
- trigger_event writes a log row with kind = the event name (Record → matcher fires),
  session-less; answers the row id. No protocol change.
- get_messages: `range` optional positional (`5:10`, `:10`, `5:`, `-5:`); omitted = all.
- del_subscribe deletes by (caller session, name).

## Touch list
- protocol/src/message.rs: ArgSpec.positional; client_commands() rewrite; spec_arg.
- cli/src/commands.rs: parse_args(tokens, spec), render_args(args, spec).
- cli/src/lib.rs: submit_tokens, message(), signature(), run_client_command,
  connect_elsewhere, cmd echo render.
- cli/src/server.rs: serve <port> [--home].
- cli/src/catalog.rs: positional value completion, session_on_line via positional,
  OfNamedArgument via first positional token.
- server/src/commands.rs: catalog (positional + 2 new commands), dispatch, subscribe,
  get_messages, trigger_event, del_subscribe.
- server/src/subscriptions.rs: named rows, persistent bash/python actions.
- server/src/store.rs: subscriptions columns name/bash/python + CRUD.
- docs/notes: cli-commands, cli-catalog, cli-lib, server-commands, server-subscriptions,
  protocol-message.
- ship skills with old syntax: subagent (SKILL+spawn.py), heir (SKILL+transcript.py),
  monitor, self-control, remote, prefix-cache → publish via scripts/ship.sh.

## Status (done, 3 commits + ship 3fe2d2c)
- [x] protocol: ArgSpec.positional; client_commands positional
- [x] cli: spec-aware parse_args (positionals + flags + diagnosis), signature(),
      message() expansion, positional completion + OfNamedArgument via first
      positional, session_named, serve <port>
- [x] server: server_catalog positional + subscribe/del_subscribe/trigger_event,
      get_messages range optional, tool_cmd.rs (registry-generated tool cmds,
      shared runner), subscriptions named actions in the session's interpreters,
      store columns name/bash/cmd/python/code
- [x] docs: cli-commands, cli-catalog, cli-lib, cli-server, cli-bin-gongga,
      protocol-message, server-commands, server-subscriptions, server-tool-cmd.md
- [x] ship skills + publish (3fe2d2c): subagent spawn.py + SKILL, heir
      transcript.py + SKILL, monitor, self-control, remote, prefix-cache
- [x] verify: build/fmt/comments(0)/test-layout(0)/skill_contract/ship --check all
      green; L3: cmd_selftest PASS 16, subagent_selftest PASS 27, prompt PASS 5.
      Recorded in .dev/verify/<sha>-cmd.json.

## Follow-ups (noted, not done)
- SPEC lists per-call `max_output` on every tool; tools still cap at the
  registry-level config. The tunnel/catalog work does not depend on it.
- Tool schemas name the timeout argument `timeout_ms`; SPEC spells it `time_out`.
