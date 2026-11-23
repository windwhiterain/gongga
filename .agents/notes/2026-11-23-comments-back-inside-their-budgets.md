# Comments brought back inside their budgets

2026-11-23 · session `comments` · branch `feature/comments-clean` · merged commit `a56fd434`

## What was wrong

`comments-check --config comments-check.toml` on `6cab1377` reported **110 violations in 26
files**: 106 `section-too-long`, and 4 `heading-not-allowed` — inline `//` comments that had
grown an `# Invariant` heading, which only doc comments (`///`, `//!`) may carry. The notes
show the count only ever drifting up across sessions (105 → 109 → 110), which is the shape of
a gate nobody empties: each session added a few and compared against the last baseline.

The budget in `comments-check.toml` is deliberate: a preamble is 120 characters and a headed
section 400, and the config's own remedy says anything longer belongs in `docs/notes`, linked
instead of duplicated next to the code.

## What was done

Every violating comment was cut to the claim it makes. Nothing was moved into `docs/notes`
and no new note was written: the prose that was dropped was restating the code, the history of
how a bug was found, or the reason a constant has the value it has — none of it an invariant
the code cannot state.

Two comments cited notes that **do not exist** (`docs/notes/bindir.md`,
`docs/notes/client-document.md`; `docs/notes/server-home.md` too, in `config.rs`). Those links
were dropped rather than kept. Five other cited notes do exist and those links stay.

One real defect surfaced: `store.rs` had **two doc blocks run together** at `plan_text` — the
tail of an old "index line" comment had lost its newline and merged into the "plan line" one,
producing a sentence that started `index_cached what` and stopped. They were one comment about
one function; the surviving text describes `plan` and records that rows stamped before the
rename carry `index_explicit`/`index_cached` as the same lists.

`channel.rs` had an `# Invariant` heading on an inline comment (fatal under the template) and
a matching over-long body: rewritten as a preamble, the invariant statement survives verbatim
in shorter form.

## Measurements

`110 → 0`, exit 0. Formula for the budget, calibrated against the checker: a section's
characters are the sum of its lines' text plus 2 per non-blank line and 1 per blank, minus 1,
with the heading line itself counted in its own section.

Gates on `a56fd434`, all exit 0: `comments-check` (0 violations), `mbx build`, `mbx fmt --check`,
`test-layout-check` (0), `scripts/skill_contract.sh`, `mbx test -p gongga-server --lib`
(40 passed, 0 failed), `scripts/ship.sh --check --local` (protocol 16 = 16, in agreement —
run from the main checkout because this worktree holds no `ship` submodule, and no ship path
changed). Record: `.dev/verify/a56fd434-comments.json`.

## Merged

The branch is **merged into `main`** as `9d363f23`. The feature branch and this session's
worktree are deleted.

`main` was not a fast-forward target twice over: it gained three note commits, then the
`null_as_empty` work (`84bcbbeb`, `50db9e0c`) in `llm.rs` — the same file as some of my
comment edits, in a different region (mine at ~200/752/809/974, that change at ~1616), so the
merge auto-merged cleanly. `main` was clean both times and the merge landed on it directly.

Gates on the merge result (`9d363f23`, run in the worktree before the merge): `comments-check`
0 violations, `mbx fmt --check`, `mbx build`, `mbx test -p gongga-server --lib` (40 passed),
`test-layout-check` (0), `scripts/skill_contract.sh` — all exit 0. On `main` afterwards,
`comments-check` is 0; `mbx build` there fails with **exit 101, zero compile errors** — `target\debug\gongga-tui.exe` is held open by the running TUI (pid 82820, the live session, with
its server on :9700), which the linker cannot replace. That is a file lock on a checkout whose
binary is in use, not a defect in this merge, and the same build passed in the worktree.

Nothing else is left behind: no worktree, no feature branch, and the scratch helpers live in
`.dev/verify` (gitignored).