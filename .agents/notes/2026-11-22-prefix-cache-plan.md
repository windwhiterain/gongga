# prefix_cache_plan: the session switch and the plan rename

2026-11-22, worktree `feature/prefix-cache-plan` (from main `454f542c`).
User instruction: add `SessionConfig.prefix_cache_plan: bool = true` and rename the
prefix-cache "index" concept to "plan", so a small-context model can skip the one-shot
meta head injection (the plan is stamped once and no `memorize` can mask it).

## What landed

- `prefix_cache_plan` through the whole plumbing: SPEC `SessionConfig`, `[agent]`
  default (true), `FileAgent` + env `GONGGA_PREFIX_CACHE_PLAN`, `Tuning`, sessions
  column + migration + clone placement, `set_session_prefix_cache_plan`, flat flag and
  nested `--config` leaf (`session_config.rs` flatten + schema). Read when the head is
  stamped, never re-rendered: a later switch moves only sessions with no head yet.
- The gate in `agent::prompt_in_force`: false skips `plan_of` entirely (planned meta
  AND rules — rules reach the model only through the plan; `agents_md` stays a
  reader-only record).
- Bug found by the L3 run and fixed: `expand_message` assumed a plan to come when no
  prompt row existed (`rooted=None`), so an opted-out session's first message kept its
  `@file` mentions literal while the head was stamped with no plan — the bodies reached
  the model nowhere. An opted-out session now assumes an empty plan and the reference
  inlines into the message (`commands.rs`, commit `a3c0b1c5`).
- Rename index→plan: `Prompt.plan/plan_explicit/plan_cached` (serde aliases read old
  rows), `Kind::PlanRow` (stored kind string stays `index`), render label `plan:` with
  old-key fallback, `plan_of`/`plan_tolerance`/`plan_framing`/`split_plan`/`PlanTurn`/
  `PLAN_CHUNK_CHARS`/`PLAN_OPENING`/`PLAN_SEPARATOR`, `plan_change_tolerance`
  (serde alias + old env fallback), labels `"prompt plan"`. Template FILENAMES kept
  (`reference_index`, `index_opening`, `index_separator`) — content identity, no
  protocol bump. Docs prose renamed in 17 files; generic "index" (UI rows, git lock,
  block indices, skill lookup) left alone.
- Ship content published: `ee43ee5` on gongga-ship main — the opening now says "The
  plan of this directory's rules and references follows." plus comment/prose renames.
  One-time prefix invalidation for every session, inherent to the rename.

## Verification

- `mbx build` exit 0; `mbx fmt --check` exit 0; `test-layout-check` 0 violations;
  comments-check 105 = baseline 105 (main itself is red; zero new violations).
- Influenced tests 8/8 (`head plan split prompt card tools_row`), including the
  legacy-spelling test: old `index` JSON keys render under the new `plan:` label.
- L3: `.dev/pcp/verify.sh` — one temp-home server lifecycle, four scenarios, all PASS
  at `a3c0b1c53f41`: off → empty plan row + body inlined in the message; default →
  plan row carries the body + mention literal; on→off and off→on after stamping →
  head byte-identical. Evidence: `.dev/verify/a3c0b1c53f41-prefix-cache-plan.json`.
  The script kills only its own lock pid + supervisor (CommandLine matched to the
  temp home); no process remains.

## Pre-existing drift found (NOT from this feature) — needs the user's decision

`scripts/ship.sh --check` exits 1: this build reads protocol **16**, origin/main
content speaks **15**. Cause: main's HEAD `454f542c` ("cwd names the machine", another
session, today 21:14) bumped `crates/server/ship.toml` 15→16, and its content
counterpart was prepared but never pushed: the main repo's ship checkout has branch
**`protocol16-preserve`** = `e127ad9` "protocol 16: a path is the only thing that names
a machine", sitting on the old origin/main `8a84271`. All 33 REQUIRED templates exist
in content 15, so the drift is the declaration, not a missing template — but a fresh
home on a 16-reading build finds no 16-speaking commit on origin and refuses to start.

The L3 script worked around it locally (no push): seed the temp home's checkout from
`protocol16-preserve` + this feature's prose commit cherry-picked, branch named `local`.

Options for the user:
1. Publish `protocol16-preserve` rebased onto the new origin/main (which now carries
   this feature's prose) — completes the other session's release; config.toml regions
   differ, rebase should be clean.
2. Leave it to the session that prepared it; this feature's branch merges regardless
   (its own content needs no bump).

## Merge state

Branch `feature/prefix-cache-plan`, squashed onto `454f542c`. The main repo's working
tree holds another session's uncommitted WIP (SPEC.md, commands.rs, docs) — a merge
that touches those files is refused by git; see the turn report.
