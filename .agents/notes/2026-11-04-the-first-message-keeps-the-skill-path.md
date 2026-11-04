# The first message keeps the skill's path

Date: 2026-11-04

The user's report, on session `polity`: "我的初始提示词明明提到了 `@skill: polity`，但好像没有
被 prefix-cache 机制自动注入，它还重新去找了一遍".

Half of that was a misreading, and the other half was a defect this repository had already
met once. The body *was* injected; the path was not, and the path is what the model went
looking for.

## What was measured

Session `polity` (id 81, `~/.gongga/gongga.db`):

- msg 66343 (seq 1), the stored `content`: `@skill: polity  现在已经有monitor了…` -- no
  `(path)`, and no body.
- msg 66344 (seq 2), the prompt row: `index` is 49,968 characters, and its
  `@skill:polity` entry is 8,710 characters byte-identical to
  `~/.agents/skills/polity/SKILL.md`; `rooted` carries `@skill:polity`.
- that turn's step 0 `usage`: `input=18208, cached=0`. The instructions are 431
  characters and the tail 98, so the index is the only thing that explains the count:
  it was sent.
- the model's own first reasoning says so: "The `@skill:polity` reference was expanded in
  my system prompt, so the skill body was included." It never re-read `SKILL.md`.

What it did instead was four lookups for the *directory*: `ls .agents/skills/polity`
(no such file), `ls .agents/skills` (only `evolve`), `ls ship/skills` (no `polity`),
`index_skill`, then `~/.agents/skills/polity`.

## The cause

`meta::expand` skipped every mention whose spelling the prompt's index carried
(`meta.rs:321`), and `commands::expand_message` counted the message's own roots as
indexed on the first turn (the index is built from those roots). The index carries a
skill's *body* and nothing about where the file lives, so the skip dropped exactly the
`@skill: <name> (<path>)` that SPEC's `## Meta` says to append. The 2026-11-03 fix
(`34b7123`) covered only the later message, which is not in the index; the first one
kept losing the path.

## What changed

- `meta::expand` no longer takes `indexed`: a `@skill:` mention always expands to
  `@skill: <name> (<SKILL.md>)`, whether or not the index carries the body. The
  spelling survives, so the message still keys the index entry.
- `meta::literal` is gone -- it existed only to feed that parameter -- and
  `commands::expand_message` no longer reads the prompt row or walks the roots.
- `docs/notes/server-meta.md` and the comments in `agent.rs` / `meta.rs` say the
  pointer is what the first message gets, and why (`@file:` is a path already;
  `@image:` is never indexed).

## How it was checked

`.dev/index-reach/probe.sh` (its CLI calls were stale -- `--session`/`--message` became
positional, so it never started a turn; fixed in place), one server lifecycle per run:

| | stored row of the first message | index after it |
| --- | --- | --- |
| before | `start with @file: early.md and @skill:chain` | carries `EARLY`, `CHAIN body`, `LEAF skill body`; `rooted = ['@file:early.md', '@skill:chain', '@skill:leaf']` |
| after | `start with @file: early.md and @skill: chain (C:\\resource\\gongga\\.dev\\index-reach\\project\\.agents\\skills\\chain\\SKILL.md)` | identical |

`PASS · index-reach · 968f651` on both runs. The later message is unchanged
(`and @skill: late (C:/…\\late\\SKILL.md)`).

Gates, run in `.worktrees/cache-fix` on `feature/skill-path-in-index`:

- `cargo build -p gongga-server -p gongga-cli` -- clean
- `cargo test --workspace` -- 8 tests, 0 failed
- `cargo fmt --check` -- exit 0
- `comments-check --config comments-check.toml` -- 0 violations (baseline on `main`: 0)
- `test-layout-check --config test-layout.toml` -- 0 violations
- `scripts/skill_contract.sh` -- PASS
- `scripts/ship.sh --check` (from the main worktree, whose `ship/` is the checkout) --
  `protocol content 1, this build 1`, exit 0

## The content that described the old behaviour

`ship` commit `fcb7668` (published to `main`; `scripts/ship.sh --check` exit 0) carries
the prose half of this change and two corrections found beside it:

- `prefix-cache`: the first message's `@skill:` row now says the mention carries
  `(<path>)`, and the later-message row says both kinds stay out of the index.
- `prefix-cache`: the section that said the index is rebuilt on every step says it is
  written once and never re-rendered -- what `prompt_in_force` and `subagent` already
  said, and what session `polity` shows (one `system` row).
- `heir`, `image`, `remote`, `self-control`, `subagent`: they found their own directory
  through the old `SKILL.md: <path>` expansion form; the path is the parenthesised one
  now.

A running server installs it at its next start, not with this build.

## Not done

- SPEC's `@image:` clause still says the mention keeps the path and gains the id; the
  code keeps the mention and the copy's id, not the payload. Untouched.

## A stale claim met on the way

`docs/notes/server-meta.md` says an edited root "reaches the next turn -- at the price the
index always charges for an edit: a new prompt row". It does not: `prompt_in_force`
returns the session's one prompt row from then on (`store::prompt` reads the latest
`system` row), so a session's index is frozen at its first request. Measured on session
`polity`: exactly one `system` row. The paragraph was left as it is -- it is a different
question from this defect -- but it is wrong today.
