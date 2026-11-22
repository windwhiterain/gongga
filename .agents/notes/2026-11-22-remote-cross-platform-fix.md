# Remote payload platform and remote-skill verdict

2026-11-22, session `kv-mask/remote-fix`, branch `fix/remote-cross-platform`.

## Root causes and fixes

### Automatic payload placement

`crates/server/src/remote/pick.rs::place` selected only the executable suffix from the
peer's home spelling. It then either accepted the presence of the protocol directory or
copied `local_payload()` -- the Windows server's own runner/server/relay -- without asking
whether the peer could execute those bytes.

`ensure_platform` now runs before the home lookup, directory creation or any copy. It maps
`uname -s -m` to Rust's OS/architecture names and refuses an unidentified or different
platform. The refusal names both platforms and the matching-distribution remedy. The
configured runner is still greeted first, so an already installed native Linux distribution
works without any Windows artifact. No existing payload is deleted or replaced.

### Remote skill verdict

`ship/skills/remote/_remote.py::server_verdict` probed `list_paths` with an empty path,
which only has a current-directory meaning for a local endpoint. It also looked for a
top-level `cmd_result`, while the CLI prints `event`/`row` envelopes. It now resolves the
configured peer's real home, passes it as `path`, unwraps events with the shipped Python
client's `gongga._event`, and reports stderr/stdout when no result frame exists. A refused
`cmd_result` retains the server's `message` instead of becoming "the server did not answer".

Real-process verification exposed one additional launcher fault on Windows:
`cli_command` chose `gongga-cli.exe`, which is the pipe client. It now invokes the server's
`gongga-cli.cmd` through `COMSPEC` on Windows; POSIX keeps the extensionless launcher.

The content change is commit `a8bf16254e384508c2eac9e626de094a744f3f61` on
`gongga-ship/main`. Both protocol declarations remain 14.

## Observed verification

Data: `.dev/verify/e2f775f6-remote-cross-platform.json` and
`.dev/verify/e2f775f6-gates.json`. The L3 run used one isolated Windows server and home,
with local content/community clones, against `whting@192.168.1.3`; it did not restart,
cancel, evolve or point a bundle at the user's server.

- `PASS · 24s · e2f775f6`.
- Automatic `exec` exited 1 with a real `cmd_result`: `this build cannot place its
  windows/x86_64 payload on a linux/x86_64 peer`. SHA-256 plus path listings for every file
  under the peer's `~/.gongga/payload` were identical before and after.
- The configured preserved Linux runner listed `/home/whting`: exit 0, 22 entries.
- Direct `server_verdict` returned the automatic refusal with `asked: true` and the actual
  `did not list "/home/whting"` message, not the old no-answer fallback. The configured
  verdict was `{asked: true, usable: true}`.
- Full `list_remotes(check=True)` reported the configured row connected, greeting protocol
  5, and `server: {asked: true, usable: true}`.
- Test-runner PIDs came from greetings; local server identity came from its temporary home
  and `server.lock`. The successful run left no local test process.

## Gate exits

- `mbx build`: 0 (55.37 s; 213 cache hits, 47 misses). Existing unrelated warnings remain;
  the new code emitted none.
- `mbx fmt --check`: 0. `git diff --check` in both repositories: 0.
- `comments-check`: baseline `fd7fb0e7` exit 1 and current `e2f775f6` exit 1; all counts,
  per-file folds and violations are exactly identical. Both have 101 existing findings
  (2 heading, 99 length) across 26 files; 146 files checked. No new finding.
- `test-layout-check`: 0; one workspace, 12 packages, zero violations.
- `scripts/skill_contract.sh`: 0; 11 skills pass.
- `python ship/python/schemas.py --check ship/skills`: 0; public schema unchanged.
- `scripts/ship.sh -m`: 0; commit rebased and pushed.
- `scripts/ship.sh --check`: 0; content `a8bf162...`, protocol 14/14, published and in agreement.

No unit tests were added or run, as required. The remote empty-path asymmetry in
`SessionManager::list_paths` remains: the skill now always supplies a real home, so fixing
that separate behavior was unnecessary. `remotes.toml`, the bad payload and the good Linux
payload were not changed. The feature branch was not merged into the user's dirty main
worktree.
