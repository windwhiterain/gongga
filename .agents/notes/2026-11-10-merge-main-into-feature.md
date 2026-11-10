# 2026-11-10 · merge main (8b4bbfd) into feature/client-split

Earlier today an attempt was made to merge on the **main checkout**. That was
wrong: `git merge --no-commit` put 54 staged files and an unresolved conflict
into the user's working tree, and a compile gate was about to run against a
`target/debug` that their live server and TUI were using. It was reverted with
`git merge --abort`; the main checkout was left clean at `8b4bbfd`, no commit,
no branch move, no process killed. This note records the merge done **only** in
`.worktrees/client-split`.

## What came in

`8b4bbfd` and the six commits before it: SPEC's Python Cmd and argument names,
`cancel` / `wait` semantics (`interrupted` -> `canceled`, `waiting_until`),
protocol 9, the `tools::skill` / `channel` / `python` side, and updates to most
`*_selftest.sh` scripts.

## Conflict

`crates/server/src/store.rs`, one line. `feature` had only rustfmt-wrapped
`conn.prepare(...)` on the old `interrupted` column; `main` renamed the column
and the function to `canceled_sessions` and added `waiting_sessions`. Resolution:
keep main's `canceled` SQL and main's function names, keep the wrapped layout.
Everything else auto-merged.

## Three follow-ups the merge itself required

1. `crates/server/ship.toml` goes 8 -> 9. The feature worktree's `ship` content
   repo is a separate gitdir and had no protocol-9 object, so `ebde89a` was
   fetched locally from the main checkout's `.git/modules/ship` and checked out
   **in the feature worktree only**. The gitlink stays `521301b`, unchanged,
   which is exactly main's current shape (`ignore = all`).
2. `docs/notes/server-commands.md` arrived from main with a trailing blank line;
   `git diff --cached --check` refuses it. One blank line removed, no prose
   touched.
3. `scripts/multi_client_selftest.sh` still sent
   `/set_session --name ...`. Main renamed that argument to `--id`
   (`crates/server/src/commands.rs`, `session_config_args`). Without the fix the
   three `set_session` commands were refused before any session existed, so the
   five `probe`-based checks failed while the other 22 passed. Three `--name`
   replaced with `--id`; 27/27 pass. `client_state_selftest.sh` already used
   `--id` and needed no change.

## Gates (all in the feature worktree)

`cargo fmt --check`, `cargo build` (0), `cargo check --workspace` (0),
`cargo test -p gongga-client --lib` (6/6), `tools::text::tests` (0),
`git diff --check` (staged + worktree), `test-layout-check` (0 violations),
`skill_contract.sh` (PASS 8), `ship.sh --check --local` (0, protocol 9/9),
`ship.sh --check` (0, remote answered), `client_state_selftest.sh -v` (PASS 4s),
`multi_client_selftest.sh -v` (PASS 13s, 27 checks),
`tui_live.py` custom draw+wheel script (exit 0, 295 frames).

`comments-check` returned 1: 12 violations, and a file-by-file comparison with
`git show main:<file>` gives exactly the same counts (2/3/3/1/3). The merge adds
0. `cargo build` prints 14 dead-code warnings from
`crates/server/src/tools/detached.rs`, byte-identical to main.

## Note on main's continued movement

While the gates ran, main advanced again to `2944263` with two TUI commits
(`crates/tui/src/vt_input.rs`, `crates/tui/src/lib.rs`). Those overlap this
branch's `lib.rs`. Whether to fold them in is a separate decision; this merge
is deliberately pinned at `8b4bbfd` so the verified result is reproducible.
