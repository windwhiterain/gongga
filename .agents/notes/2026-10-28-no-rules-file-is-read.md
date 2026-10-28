# No rules file is read

User's instruction, 2026-10-28: the harness must auto-import no `AGENTS.md`, and the
machine's own `~/.agents/AGENTS.md` becomes a local skill under `~/.agents/skills/`.
Named `whiting-dev`.

## What left the binary

`prompts::constitution` joined two files into `instructions` whenever a prompt row was
written: `~/.agents/agents.md`, then `AGENTS.md` beside the session's reported cwd. Both
reads, `machine_file`, the `constitution` name in `REQUIRED`, and its caller in
`agent::prompt_in_force` are gone. The template itself is gone from `ship/config/config.toml`.

The boundary those reads sat on stays: `prompt_moved` still notices a different directory at
a turn's first step, because the stored row names where the shell stood. A row is still
rendered once and replayed as stored. Nothing about replay fidelity changed -- only what was
in the text.

## Why a skill and not a read

The argument the skill root already makes, applied to the one prose that ignored it: a rules
file was in every request whether or not the turn needed it, and editing it changed what the
model was told without any row recording the user saying it. A skill costs a name and a
description until the work asks for the body -- and `index_skill` scans the roots at call
time, so `~/.agents/skills/whiting-dev/SKILL.md` is listed the moment it exists, with no
reinstall and no pin move.

## Verified

- `grep -rniE 'agents\.md|constitution' crates/server/src` returns nothing: no code path
  reads one.
- `cargo test --workspace` 223 passed (was 224 -- the deleted test was the rules-join one),
  `test-layout-check` 0 violations, `comments-check` clean.
- The skill's frontmatter is the shape `index_skill::frontmatter` parses: `---` first,
  single-line `name:` and `description:`.
- `~/.agents/AGENTS.md` is moved to `~/.agents/AGENTS.md.pre-whiting-dev.bak`, so it is out
  of every import path and one rename from coming back.

## Open

- This repository's own `AGENTS.md` -- SPEC-is-truth, the gates, the worktree-home rule,
  the ship and evolve workflow -- was reaching gongga sessions through the read that is
  now gone. It needs the same treatment, or the sentences have to move into `instructions`.
  Not decided.
- The ship commit and the pin are separate from the Rust commit: `ship.toml` still names the
  pushed commit, so a home runs the old prose until `scripts/ship.sh` publishes and `evolve`
  swaps. An installed home that still carries a `constitution` key is inert -- nothing asks
  for that name.
