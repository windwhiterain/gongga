# Polity subscribe callback update

Date: 2026-11-05. Session `polity`.

Goal: update the community `polity` skill for the subscription mechanism where a
match runs only its configured action and no longer injects a notice by itself.

- Source: `~/.agents/gongga-agents`, existing session worktree `.worktrees/work`.
- Branch: `fix/polity-subscribe-action`, based on `main` / `origin/main` at `792184c`.
- Implementation: arm the composed watch with a dedicated bash callback that sends
  `GONGGA_EVENT_KIND`, `GONGGA_EVENT_SESSION`, and `GONGGA_EVENT_PAYLOAD` to
  `$GONGGA_SESSION`, matching the existing `subagent` relay.
- Documentation: remove the old automatic-injection wording and show the real
  callback and message shape.
- Tests: the user explicitly instructed deleting the old exact subscribe-argument
  assertion; do not add tests. Baseline before edits: `python skills/polity/test_polity.py`,
  2 tests passed.
- Main Gongga checkout has unrelated dirty Rust/TUI files; do not touch them.
- Do not push without the user's instruction.

Outcome:

- Commit `be2c951` (`polity: relay subscription matches to justice`) is on `main`;
  `local` was rebased on it as `9d1e725`, and the feature branch was deleted.
- `python skills/polity/test_polity.py`: 2 tests passed after the user-requested
  assertion deletion.
- `bash scripts/subscription_selftest.sh`: PASS, all 7 scenarios, 18.3 seconds,
  server build `e668ca871774eee6752baa20183d5a33fb329aac`. Machine-readable result:
  `.dev/verify/e668ca871774eee6752baa20183d5a33fb329aac-subscription.json`.
- Not pushed; `origin/main` remains `792184c`.
