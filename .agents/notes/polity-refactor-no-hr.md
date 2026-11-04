# Polity refactor: HR out, monitor + one composed subscribe in

Date: 2026-11-04. Session `polity`.

- `gongga-agents` (community clone, `~/.agents/gongga-agents`): on `pub` in
  `.worktrees/work` → committed `aa221c7` (skills/polity: SKILL.md, polity.py,
  test_polity.py). Brought into the main checkout: `main` fast-forwarded to pub,
  `local` rebased; the server now serves the refactored skill.
  - `start` creates only advisor + premier, arms justice's watch
    (`subscribe polity-<task> '^turn_end:polity-<task>-(advisor|premier)$'`)
    before briefing; failure to arm deletes the created sessions.
  - `--justice` flag and the `remodel` command are gone: subscribe notices the
    caller, and quota/harness repair belongs to the server's `@monitor`.
  - Roles end their turn with the record id as the answer; the notice carries
    it to justice.
- `gongga` repo: `84cba05` on main — SPEC.md Skill: Polity drops the HR bullet
  (states what replaces it) and the subscribe command documents its pattern
  language (regex over event name / `name:session`).

Not done (needs the user's call): pushing. Documented commands:

```bash
cd ~/.agents/gongga-agents/.worktrees/work && git push origin pub:main
cd /c/resource/gongga && git push origin main
```

Verified: skill unit tests (2, green in both worktree and served location),
`gongga-cli skill which polity` resolves to the served entry,
`scripts/skill_contract.sh` PASS, `scripts/ship.sh --check --local` in agreement.
Not run: cargo gates (no Rust touched — SPEC.md and skill files only).
