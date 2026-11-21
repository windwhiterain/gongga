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
- Measured evidence: `.dev/verify/84ada112-live-compact-continue.json`, keyed by
  the merged commit. It supersedes `be1ef60d-…` and `74bc821c-…`, which were
  measured on the branch and lived in the feature worktree that has since been
  removed. The driver was `.dev/verify/live-compact-continue/drive.sh` (with
  `fake_seq.py`, a scripted endpoint that asks `exec`, then `live_compact`, then
  answers) and went with that worktree; recreate it from SPEC if the code changes.

## Verified (real processes, both modes, twice: pre- and post-rebase)

| mode | checkpoint | turn_end | requests | resume mark |
| --- | --- | --- | --- | --- |
| omitted | 1 | done | 3 (the 3rd carries `next`) | 0 |
| `continue=false` | 1 | done | 2 (none after the checkpoint) | 0 |

Gates, re-run on the rebased head `cbe36cfd` (main `e078312b`): `mbx build` 0,
`mbx fmt --check` 0, `mbx test -p gongga-server --lib` 40 passed / 0 failed,
`test-layout-check` 0 violations, `skill_contract.sh` PASS, `ship.sh --check
--local` 0 from `C:/resource/gongga` (content HEAD `03d44c18`, protocol 14 = 14 --
the check verifies nothing from a worktree whose `ship` is not a checkout), and
`comments-check` no new violations: `store.rs` and `agent.rs` each carry the same
single pre-existing "the preamble" violation on main, `live_compact.rs` clean.
Both e2e scenarios re-ran green on this head; no test process was left behind.

## Landed

- Merged into `main` as `84ada112`, a single squashed commit, and the branch
  `feature/live-compact-continue` and its worktree are deleted. The trees match:
  `git rev-parse cbe36cfd^{tree}` and `84ada112^{tree}` are both `5a1825b3`, which
  is the proof that the squash carries the branch's whole tree (git's own
  "fully merged" test cannot see it, so `-d` refuses and `-D` is correct here).
- The blocker recorded here earlier -- someone else's ` M SPEC.md` in
  `C:/resource/gongga` -- cleared on its own: that session committed its work as
  `3eb8d7df`, which is in `main` now. Nothing of theirs was touched.
- Still untracked and not mine:
  `.agents/notes/2026-11-21-the-wire-between-the-two-machines.md` (session
  `merge`).