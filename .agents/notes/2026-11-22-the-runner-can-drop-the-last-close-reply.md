# The runner can drop the last close reply

Date: 2026-11-22. Found while verifying the `remote` skill fix against a real Linux peer.

## What was fixed and published

`ship/skills/remote/_remote.py::peer_home` wrapped the `pwd -W` probe in `checked()`.
`pwd -W` is a Git Bash builtin; a POSIX peer exits 1, `checked` raised, and the POSIX
fallback written immediately below it was unreachable. So `install` and `verify` failed on
every Linux peer with `reading the peer home: bash: line 1: pwd: -W: invalid option`.
`check`, `configure`, `list_remotes` and `remove` never call `peer_home` and were unaffected.

Fix: the probe is no longer wrapped; its stdout is read only when it exits 0, otherwise the
POSIX branch runs. This matches the pattern `source_size` already uses in the same file.

Published as content commit `ff7d6e0`. `scripts/ship.sh --check` exit 0, protocol unchanged
(content 14, this build 14). `scripts/skill_contract.sh` PASS 11 skills.

Verified against `whting@192.168.1.3` **before** publishing, old installed copy vs new repo
copy, same peer and same runner:

- old: `Failure reading the peer home: bash: line 1: pwd: -W: invalid option`
- new: `open shell 71` answers `cwd: "/home/whting"`, then cwd set, shell state preserved
  across commands, `(exit 7)` exact status, invalid directory refused, `sleep 60` written.
  Six of the seven `runner_verify` steps pass.

## The seventh step, and what it exposed

`close shell 71` -- the only shell, busy with `sleep 60` -- is sent and **no reply frame
ever arrives**; the runner closes stdout. With `GONGGA_REMOTE_DEBUG=1` the frame log ends
at `REQUEST {'close': {'shell': 71}}`.

Mechanism candidate, `crates/server/src/remote/runner.rs::serve_stdio`: each request is
handled by `tokio::spawn(runner.clone().serve(request))`, while the read loop is

    let line = tokio::select! {
        line = lines.next_line() => line,
        _ = runner.broken.cancelled() => break,
    };

after which it calls `terminate_all()` and returns. `broken.cancel()` has a single call
site (runner.rs:148). If it fires while a `close` is still in flight, the loop breaks and
the process exits before the spawned task emits its reply. `close()` itself always builds a
report, confirmed or unknown, so the handler is not what swallows it.

This is OS-independent. Linux never reached it because `peer_home` failed first. **Not
fixed here**: it is a Rust change in the main repo and needs its own reproduction and gates.

## Consequence for using the remote

The remote is usable: greeting, open, write, output, exact exit status and invalid-cwd
refusal are all verified. Only the skill's final busy-close assertion fails. A server
restart makes `whiting_main` visible and installs the published skill fix; it does not fix
the race.

## Not reproduced, therefore not changed

The runner subagent reported `probe()` leaving the peer's runner alive after it killed the
local ssh. Four probes here (three `check`, one `list_remotes --check`) left none behind.
The structural risk is real -- `probe()` kills only the local ssh, and the runner is
long-lived by design and ignores stdin EOF -- but it was not reproduced, so that code was
left alone. The greeting does carry `pid`, which is what a precise cleanup would use.
