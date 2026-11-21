# live_compact gains `continue`

User instruction: "仿照handoff， live_compact也接受一个continue参数".

## Settled

- `SPEC.md` now reads `live_compact(settled, next, continue: bool = true, session)`.
- `store.rs::append_live_compact_result` writes `next` as `Kind::Inject` with
  `role: user` (it was `Kind::Text` / `role: assistant`), and reports the flag on
  `live_compact::Committed { continue_turn }`.
- `agent.rs`: `run_one_call` returns `CallEnd { Ran, EndsTurn, Stopped }` instead of
  a `bool`. A committed `continue=false` sets `ends_turn`, and `step_inner` returns
  `StepResult::Finished` for it **after** the stream closed and every call ran.
- `ship/config/config.toml` `[tool.live_compact]` declares the `continue` argument,
  and its `next` sentence says "stored as a user turn".
- `docs/notes/server-live-compact.md` carries the measurement and the reasoning.
- Content published: `scripts/ship.sh` pushed `e8a5a48..e2875ff` to
  `gongga-ship` main; `--check` exit 0 ("in agreement, and published").
- Measured evidence: `.dev/verify/be1ef60d-live-compact-continue.json` (the rebased
  head; it supersedes `74bc821c-…`, measured before the rebase), driver at
  `.dev/verify/live-compact-continue/drive.sh` (with `fake_seq.py`, a scripted
  endpoint that asks `exec`, then `live_compact`, then answers).

## Verified (real processes, both modes, twice: pre- and post-rebase)

| mode | checkpoint | turn_end | requests | resume mark |
| --- | --- | --- | --- | --- |
| omitted | 1 | done | 3 (the 3rd carries `next`) | 0 |
| `continue=false` | 1 | done | 2 (none after the checkpoint) | 0 |

Gates, re-run on the rebased head `be1ef60d` (main `cc1071b9`): `mbx build` 0,
`mbx fmt --check` 0, `mbx test -p gongga-server --lib` 40 passed / 0 failed,
`test-layout-check` 0 violations, `skill_contract.sh` PASS, `ship.sh --check
--local` 0 from `C:/resource/gongga` (content HEAD `03d44c18`, protocol 14 = 14 --
the check verifies nothing from a worktree whose `ship` is not a checkout), and
`comments-check` no new violations: `store.rs` and `agent.rs` each carry the same
single pre-existing "the preamble" violation on main, `live_compact.rs` clean.
Both e2e scenarios re-ran green on this head; no test process was left behind.

## Open

- **Merge into main is blocked, deliberately.** `C:/resource/gongga` carries one
  uncommitted file, ` M SPEC.md` (2 lines changed in `## Keep Alive`, mtime
  2026-11-21 11:56 UTC; blob `21bd0f48`, which matches no commit in this repo's
  history -- so it is genuine new work, not a stale copy). It belongs to session
  `merge`, which is idle after asking the user a question, so it will not commit
  on its own. Git refuses a merge that must write a file with local
  modifications, and stashing another session's WIP is not mine to do. The parent
  merges `feature/live-compact-continue` once that WIP is committed.
- No overlap: my SPEC.md edit is the `live_compact` line (around line 163), theirs
  rewrites `## Keep Alive` (around line 208), so the merge will not conflict.
- The branch is now `be1ef60d`, rebased onto `cc1071b9`, 3 ahead / 0 behind. That
  rebase mattered: `cc1071b9` rewrote `store.rs`'s head rows, the same file my
  change lives in, which is why the e2e was re-run instead of cited.