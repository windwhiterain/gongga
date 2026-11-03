# A skill reference keeps its spelling

Date: 2026-11-03

User's instruction: "按照SPEC的要求修复这个问题" — the defect being that `@skill: <name>` did
not become `@skill: <name> (path)` as SPEC's `## Meta` says, and that the path was
reported as reachable only through `index_skill`.

## What was wrong, measured

Both halves of the defect are one line of a stored row, at `52b3c7a`:

    now read @file: late.md andskill `late` (project) A skill a later message names.;
    SKILL.md: C:/.../.dev/index-reach/project\\.agents\\skills\\late\\SKILL.md

Two faults, not one:

1. `meta::skill_metadata` replaced the whole mention with prose (bucket, description,
   path). SPEC says append the path to the mention, so the mention survived nothing and
   the spelling that keys the prompt index was thrown away. Consequence, measured on the
   same run: `@skill:late` named in a later message never reached the index
   (`seq 11 index carries:` had no `LATE skill body`), while the row kept a description
   that goes stale the moment the skill changes. `@file:` never had this problem,
   because it is left alone.
2. The space in front of a mention was eaten whenever the mention stood between spaces,
   whatever the replacement was. `and @skill: late` became `andskill`. The rule exists
   for an empty replacement -- an image leaves a hole and the sentence should close up
   around it -- and was applied to one that carries text.

## What changed

- `meta::skill_metadata` returns `@skill: <name> (<path of its SKILL.md>)`, the spelling
  first, so the index keys the same entry the message wrote.
- `meta::expand` takes the preceding space only when the replacement is empty.

## How it was checked

One server lifecycle, one script, `.dev/index-reach/probe.sh` (a fake provider that
reports a big request, so the `Context notice` crosses too), before and after:

| | stored row for the later message | index after the later message |
| --- | --- | --- |
| before | `andskill \`late\` (project) A skill a later message names.; SKILL.md: <path>` | no `LATE skill body` |
| after | `and @skill: late (<path>)` | carries `LATE skill body`, `@skill:late` in `rooted` |

Verdict lines: `PASS · index-reach · 52b3c7a` (both runs), and the change is on
`feature/skill-meta-path`. `cargo test --workspace` is 12 binaries with 0 tests in this
repository, so the L3 script is the whole oracle here, not a supplement to it.

## Not done, and why

- `ship/skills/prefix-cache/SKILL.md` and `ship/skills/subagent/SKILL.md` still say a
  later `@skill:` is "被就地替换成一行 metadata，永远不进 index". That is content in the
  submodule, and it needs a release; this checkout's `crates/server/ship.toml`, `ship/`
  gitlink and `scripts/ship.sh` are all mid-migration in the main worktree by another
  agent, so a release from here would collide. The two paragraphs are the follow-up.
- SPEC's `@image:` clause (`append id to it @image: <path> (id)`) is still not what the
  code does: the mention is removed and the picture rides the row. Left as it is --
  changing it is a second decision about what a stored row should say, and it was not
  the reported defect.
- `scripts/ship.sh --check` exits 2 in a clean checkout of this HEAD: the gitlink records
  content commit `0c127db`, which predates the content-side `ship/ship.toml`, so `field`
  on the missing file returns 2 and `set -e` turns it into a silent usage exit. Checked
  against the published content (`c1d3b70`, after a `fetch`): `protocol content 1, this
  build 1`, `exit=0`. Pre-existing and not touched by this change.
