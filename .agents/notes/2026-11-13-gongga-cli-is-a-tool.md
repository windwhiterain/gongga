# `gongga_cli` as a tool, beside the direct route

SPEC's `## Tool` says a command is run by `fn gongga_cli(cmd: Cmd)`. The direct route a
`[tool.<name>] cmd = "<a command>"` row offers stays -- some tools are registered that
way, and deleting it would delete the rows that exist -- so this build has two shapes of
one door, and the tool list gains `gongga_cli`.

## What was built

- `crates/server/src/tools/mod.rs`: `GonggaCli`, a `Cmd` whose command name comes from the
  call instead of from the row. It reads `cmd` with `route()` and runs it through
  `run_command()` -- the same call `Forward` makes, so the caller's session rides along
  and a name nobody answers is refused by the command layer in its own words.
- `route()` takes the object or the words of one (a call may spell a table as its JSON),
  names the command from `cmd.name`, and reads `cmd.args` with `words_of` -- optional,
  because a command need take none. `words_of` is now the identity on a call's arguments:
  SPEC's `Cmd.args` is `dict[str, any]`, so a number stays a number and a stated `null`
  stays the type's empty value (this is the rule `null-fix` landed: `aebd3ece`, `6cd4f7c1`).
  `named_words` is that less the `cmd` key, which in a row names the command and never one
  of its arguments.
- Content (`ship`): `[tool.gongga_cli]` -- `offered`, `carrier = "here"`,
  `tunneled = false`, one required `cmd` argument of SPEC's type `Cmd`; and the `exec`
  tool's `args` sentence now names `gongga_cli` instead of "a cmd is called by its own
  name as a tool".
- `ship/skills/self-control/SKILL.md`: "Four doors, one catalog" -- the tool that takes the
  command itself first, then the row named after the command, the shell's
  `gongga-cli '<json>'`, and Python.
- `docs/notes/server-tools-mod.md`: the section that said SPEC's tool surface was `exec`
  and `wait` with everything else `fn <tool>(**args)` now says what SPEC says, and a new
  section says what the second shape is for: a config cannot enumerate the commands a
  skill teaches.
- Three stale `fn <tool>(**args)` references -- the published `subscribe` description in
  `commands.rs`, a `commands.rs` doc line about `exec`, and `subscriptions.rs` -- now name
  `gongga_cli`.

## How it was checked

- `cargo build -p gongga-server -p gongga-client`: clean. A workspace-wide `cargo build`
  cannot remove `target/debug/gongga-tui.exe`: a TUI of the user's is running it.
- `comments-check` on `tools/mod.rs`: the two violations HEAD already had, none added.
  `test-layout-check`: 0 violations. `scripts/skill_contract.sh`: PASS, 9 skills.
  `scripts/tool_selftest.sh`: PASS, 29s.
- One real server on a home built from the live home's `config.toml` plus the published
  row (a *fresh* home cannot start: `ship/config/config.toml` ships
  `instructions = ''''''`, which predates this change):
  - the stamped `tools` row the model is handed lists `gongga_cli`, its one argument
    `cmd` of type `object`, required;
  - `ls_channel`, `send_message`, `get_messages`, `ls_session` ran through the tool, and
    `subscribe` carried a nested `cmd` table and a boolean `persistant` as themselves;
  - an unknown command answered `unknown command "no_such_cmd"` in the command layer's
    words, and `{"cmd": 42}` was refused by the schema check before the route ran;
  - the same command through its own row (`ls_channel`) answered the same, so the two
    doors agree.
  The home, its processes and its checkout were removed afterwards.

## A gap this route inherits, not one it made

`resolve()` in `commands.rs` counts a `session` argument as missing when the call does
not state one, so `{"cmd": {"name": "ls_channel", "args": {}}}` is refused with
`ls_channel is missing required argument(s): session` even though the handler already
reads the caller (`session_of`, `Origin::Tool`) and SPEC writes `session: Session =
caller()`. The row-named door is refused the same way, so this is the command layer's
open item -- `6cd4f7c1` moved the caller into the origin and left the catalog's `required`
flag alone -- and not something the route can fix without deciding what `required` means
for an argument the caller fills.

## Notes

- Content: the row went out as `21e3b3e`, then `type = "Cmd"` (SPEC's spelling, now that
  `protocol::argtype` is the one table) as part of `513ebf2` -- a concurrent session in
  this checkout committed it with `git add -A` while it was in the working tree.
- A `scripts/ship.sh -m` run on this machine hung for seven minutes with no output (a
  `cmd.exe` child of it, no git process of its own) and was stopped through its channel;
  the checkout was left clean and in sync, and `--check` agrees.
