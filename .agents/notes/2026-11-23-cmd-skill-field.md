# Cmd.skill: one field instead of a wrapping command

2026-11-23. Worktree `.worktrees/cmd-skill`, branch `feature/cmd-skill`, base `5d320162`.
User's instruction (verbatim): 为了减少 run skill cmd 的嵌套层数，不再 run skill 用单独的
server cmd，而是所有 cmd 都附带 `skill: Skill = none` 字段，none 即原生 server cmd，some
就是对应 skill 的 cmd。 Fork answered **A**: keep the content's `[[command]]` declarations
(`read`/`write`/`edit` stay bare names, routed to the skill door internally).

## The change

- `protocol::Cmd { name, args, skill }` is one type, used by the dispatch, the command
  layer, a subscription's action and the `gongga_cli` tool's `cmd` reading.
- The wire envelopes (`ClientMessage::Command`, `Event::Cmd`) carry `skill` beside
  `name`/`args`; `CommandSpec` carries `skill` so a row that is a skill's function says so.
- `run_skill` is gone as a command. `skill_command(d, id, function, args, caller)` is the
  one door; `declared_command` only names the skill and function. `Skills::run` takes a
  function and its arguments, not a nested `cmd` (the `Call`/`call_of` parser is deleted).
- `OWN_CLOCK` no longer names `run_skill`: a call that states a skill keeps the skill
  runner's bound, read off `Cmd.skill`.
- `skill_commands::rows` publishes one catalog row per package function from the generated
  `schema.json` (the wheel `attach_skill_schemas` already read). A name this build already
  answers is not a skill's to take. The client fills `Cmd.skill` from the row, or from a
  `skill` key the JSON object stated; the server reads only `Cmd.skill`, so `none` stays
  literally "native".
- The shared arguments (`laws::ALL`) are taken off in the skill door, once.

## Content (main checkout's `ship/`, branch `main`)

The worktree's `ship/` is a detached worktree of the content repo, so the pre-commit
`--check --local` and the selftests can read it. Content edits land in
`C:\resource\gongga\ship` and are published once, after the build's protocol bump is on
main: `scripts/ship.sh -m ...`.

- `ship.toml` protocol 16 -> 17 on both sides: the python contract (`gongga_cli.py`,
  `gongga.py`) now sends `skill`, and the prose teaches a call shape only a 17 server
  reads. An old build keeps installing the newest 16 commit.
- Prose: `skill`, `self-control`, `image`, `handoff`, `file`, `remote`, `providers`,
  `subagent` SKILL.md; `config.toml`'s `gongga_cli` summary; `README.md`.

## Left to do

- python: `gongga_cli.py` reads `cmd.skill`; `gongga.py` `call(..., skill=...)` and the
  docstring example; `schemas.py` if it names run_skill.
- client completion: the `schema_of` table that hung on `run_skill.cmd` is gone; skill
  rows carry their own args, so `/show{...}` completes from the row.
- docs: `server-commands.md`, `server-content-commands.md`, `server-tools-skill.md`,
  `python-client.md`, `client-complete.md`, `server-channel.md`, `server-explore.md`.
- scripts: `cmd_selftest.sh`, `remote_meta_selftest.sh` call spellings (no new tests).
- SPEC.md: `Cmd := {name, args, skill}`, `## Python Cmd` loses `run_skill`.

## Verification (run on real processes, worktree build)

- `mbx build`: green (warnings are the pre-existing dead-code set, unchanged).
- `mbx fmt --check`: 0. `comments-check`: 10 errors now = 10 on baseline (all pre-existing;
  the ones this change introduced were shortened to budget). `test-layout-check`: 0.
- `scripts/skill_contract.sh`: PASS 11 skills.
- `scripts/cmd_selftest.sh`: PASS (a package skill runs via `Cmd.skill`; body/unknown refused).
- `scripts/skill_call_selftest.sh`: PASS. Fixed two latent isolation bugs it had: it now
  exports `GONGGA_SHIP_URL` (the server was pointing the checkout at GitHub mid-run) and
  `GONGGA_HOME` (the CLI shim was reading the caller's live home, whose old client dropped
  `skill`).
- `scripts/subagent_selftest.sh`: A/B/C/E green after the wrapper exports `GONGGA_SESSION`;
  the remaining 0/F/H/J failures are identical to the baseline run in the main checkout.
- `scripts/selfcontrol_selftest.sh`: F/H fail identically on baseline (pre-existing).
- `scripts/spawn_dir_selftest.sh`: scenario 6 needs a real peer runner (environmental).
- A controlled reproduction (`.dev`, since removed) confirmed `self_config` and
  `new_subagent` answer ok through the new door.

## Publish

Content committed in the main checkout's `ship/` on `main` (7bc5e35, protocol 17). The
worktree's `ship/` is a detached worktree of the content repo so the hook and selftests read
it. Merge the feature to main (manifest 17), then `scripts/ship.sh -m` pushes content, then
`--check` must answer 0.
