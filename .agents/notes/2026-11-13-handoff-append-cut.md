# handoff: adapt to the append/cut context model (2026-11-13)

## Goal
User: the `handoff` skill did not adapt to SPEC's new invariant "context only has
append/cut API". Fix it. Source of truth: `ship/skills/handoff/` (content repo).

## The bug (proven, not inferred)
- Server migration adds trigger `context_cut_tail_only` (crates/server/src/store.rs):
  BEFORE DELETE on messages, aborts when `OLD.seq < MAX(seq)` of the session.
- handoff's old cut was `DELETE ... WHERE seq < kept.seq` (delete everything BELOW the
  in-flight call). In-memory probe: that statement ABORTS ("context may only be cut at
  its tail"); even a bulk tail delete aborts (SQLite deletes ascending). Only a
  tail-first, row-by-row cut works.

## The fix
- transcript.py: cut one row at a time newest-first; the in-flight call IS the tail, so
  it is cut with the rest and appended back verbatim (call_id preserved) at seq 1.
- test_handoff.py: the kept-call test asserted the call stays at its ORIGINAL seq 2;
  under tail-only cut that is impossible, so expectations updated to seq 1 / [1,2].
  (Necessary consequence of the model change, not new test work.)
- SKILL.md: mechanism + seq rule synced to append/cut.
- Local commit d921a08 (on local main). PUBLISHED to origin/main as d252062 via
  cherry-pick onto origin/main + fast-forward push (9d2dbb3..d252062).

## Verification (reproducible)
- scripts/handoff_selftest.sh: PASS. NOTE: it extracts only the SCHEMA constant, which
  has NO triggers (they are added in migrate()), so it does not by itself exercise the
  tail-only cut. Gap: making the fixture trigger-aware needs a test change (the tamper
  test uses UPDATE, which context_no_update forbids) -> left for user authorization.
- Separate real-model run (temporary verifier under target/, since removed): real
  SCHEMA + extracted migration triggers; proved the trigger is live (below-tail delete
  aborts), then handoff PASS for kept-call (session=1 row at seq1, mentor=6) and no-call
  (session=0, mentor=4); context_tokens=0; note delivered. Ran on BOTH local and the
  cherry-picked publish state.
- scripts/skill_contract.sh: PASS. ship.sh --check --local: protocol 9==9.

## Pre-existing git mess (NOT created by this task)
- ship/ was left mid-merge: HEAD=d6a725e (local main) merging MERGE_HEAD=9d2dbb3
  (origin/main), 5 conflicted files incl. a large unrelated self-control rewrite.
  Resolving all of it is out of scope/risky, so `git merge --abort` (documented ship.sh
  recovery) restored clean local main, then the fix was applied there.
- Divergence REMAINS: local main is 4 ahead / 19 behind origin/main. Local's
  transcript.py has improvements origin lacks (command_words, rename_mentor,
  record_note_failure) -- preserved in local main, NOT yet published. `git cherry
  origin/main main` => +7c06c61, -ce5741c, +d6a725e (two non-equivalent commits).
- ship.sh --check exits 1 on this divergence (protocol IS in agreement; the fix is
  published and readable). Reconciling local main with origin/main is a separate
  decision involving the divergent commits; not forced here to avoid dropping WIP.
