# Rename heir -> handoff; organize before handing off

User ask (2026-11-12): the `heir` skill should use the familiar "handoff" wording
(low-capability flash models do not know what "heir" means), and it must NOT demand
an immediate handover: first put the documents, history/memory and references in
order, then hand off.

## Scope
- Content (`ship/`, published via `scripts/ship.sh -m`): rename `skills/heir` ->
  `skills/handoff`; subcommand `heir` -> `handoff`; "first lesson" -> "handoff note";
  rewrite `context_notice` / `size_hint` to organize-first; update cross-skill refs.
  Keep `--mentor` / `<session>/mentor` (the archive's name) and the `transcript`
  entry: the complaint is "heir", and `mentor` is referenced everywhere.
- Server: `agent.rs` / `store.rs` comments, the `live_compact_notice` "Do not use
  heir" string, the notice test fixture; rename `scripts/heir_selftest.sh` ->
  `handoff_selftest.sh`; `AGENTS.md`, evolve skill, living `docs/notes`.
- Protocol unchanged: a renamed skill is content (retire a path + add a skill), not
  a shape change. The home install retires the old `skills/heir` automatically
  (files in the old content but not the new are removed).

## Baseline finding
The published content remote `main` is `331f934` (HTTPS, what the running server
installs and what the outer gitlink records). The primary checkout's `ship` local
`main` is `bffcc42`, a DIVERGENT history (it does not contain `331f934`) with a stale
SSH tracking ref. Work is based on the clean published `331f934` in this worktree;
the primary's divergent WIP is neither published nor built on. Flagged to the user.

## Timing reversal
The 2026-10-31 design made the notice "train immediately, no prerequisites". The
user now reverses that: organize first (commit WIP, write down in-head state, gather
docs + references), then hand off. Still no need to finish the task or make tests
green; do not start new work. The prompt gate (`test_handoff.py`) is updated to
assert the new requirement instead of the old immediacy.
