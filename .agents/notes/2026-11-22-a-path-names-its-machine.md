# a path is the only thing that names a machine

2026-11-22. The user's verdict: `exec`'s `remote` goes, `new_channel` gets a `cwd`
instead, a session's `workspace.remote` folds into `workspace.cwd`, and a machine has
to be able to tell a remote path from an ordinary one. Protocol 16.

## What a path says

`remote://<name>/<path>` names a machine and a path on it, and nothing else names a
machine anywhere in a call. Three readings, one function on each side
(`crates/server/src/space.rs::machine_of`/`stated`, `ship/python/gongga_space.py`):

| written | means |
| --- | --- |
| `remote://build//home/build/x` | peer `build`, at that absolute path — the name ends at the first `/`, so a POSIX path gives two slashes in a row |
| `remote://local/<abs>` | this machine, stated |
| `remote://<name>/` | that machine and no directory: its own starting directory stands, or the session's own when the machine is its own |
| bare `x` | wherever the call already stands |

The tail of a named path must be absolute, because the only directory this process
knows is on this machine; a relative path is spelled bare. That refusal is the one that
was missing: without it the server joins this machine's directory onto a peer's path.

## Where the machine came from before

| call | then | now |
| --- | --- | --- |
| `exec` | `remote` argument beside `cwd` | `cwd` alone; `bin` is a program on that machine |
| `new_channel` | `remote` argument, no `cwd` | `cwd`, and nothing else |
| `list_paths` | `remote` + `path` | `path` |
| `set_session` / `new_session` | `workspace.remote` + `workspace.cwd` | `workspace.cwd` |

## A channel's machine is settled once

`exec` reads the machine **only when it creates a channel**: a call that speaks to one
that exists cannot move it, and a stated `cwd` on such a call says nothing. omitted
`cwd` is the session's own machine and directory.

## Measured

`.dev/verify/e755c4dc-cwd-machine.json`: fmt/build/tests/comments (104, the baseline
count)/layout/skill-contract/ship --check all green, `scripts/channel_selftest.sh`
PASS 33 s, and — against the real peer `whiting_main` — a program started with
`cwd=remote://whiting_main//home/whting` answered `Linux /home/whting`, the channel kept
that machine on a later `input`, a prefixed relative path and an unknown name were both
refused, and `set_session` moved a session there and back.

**The real peer is what caught the bug.** `Stated::tail` bound
`Stated::There(name, tail)` in the wrong order, so a peer path resolved to the machine's
name. Three local cases passed with it; only the peer showed it (`it has no directory
"whiting_main"`). That is the reason this change was not closed on unit tests.

## Left open

`scripts/spawn_dir_selftest.sh` fails 6 of 9 under a forged `HOME`: content copied
without `.git`, and a peer session with an omitted `cwd` now really reaches that peer
(it used to be a name nothing contacted). Both are the script's assumptions, not
regressions — but the scenarios need rewriting before it is a gate again. The local
channel selftest is green.