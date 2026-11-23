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

## Not done, and why

**The branch is not merged into `main`.** `main` was `6cab1377 + 3 note commits`, and its
working tree carries an **uncommitted `crates/server/src/llm.rs`** change from another session
(`null_as_empty`, for a provider that spells an empty list as `null`). A fast-forward would
overwrite it. `main` is a strict ancestor of `feature/comments-clean`, so the merge is a plain
fast-forward whenever that file is committed or stashed:

```bash
git -C C:/resource/gongga merge --ff-only feature/comments-clean
```

Nothing else is left behind: the worktree is clean, and the scratch helpers live in
`.dev/verify` (gitignored).