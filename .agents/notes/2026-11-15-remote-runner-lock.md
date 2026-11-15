# A remote session's channel call stalls every other remote call

Investigated 2026-11-15. Three sessions were reported stuck: `lichen-vm`,
`lichen-vm/templates` (both bound to `remote = lichen-vm`) and `fix` (a local
session that had called `ls_channel` on `lichen-vm/templates`).

## What was observed

| session | last event | unfinished call |
| --- | --- | --- |
| lichen-vm/templates | step_timing @03:24:08 | `exec /compute/final-all` (wait 300000) |
| lichen-vm | step_timing @03:36:01 | `gongga_cli ls_channel{session: lichen-vm/templates}` |
| fix | tool_timing @03:41:56 | `gongga_cli ls_channel{session: lichen-vm/templates}` |

- The peer's program had finished long before: its channel `status` said
  `{"exit":0,"ended_at":…296153}` (03:28:16), and `out` held the whole answer.
- The server was healthy: 28 threads, other sessions kept stepping, sqlite writable.
- The server's one live `ssh` (the runner connection) had used **0.05 s of CPU in
  35 minutes** — no requests were leaving.
- `fix` was not stuck forever: its call returned after **1710 s** and the turn
  went on. `lichen-vm` and `lichen-vm/templates` never returned.

## Root cause

`crates/server/src/tools/channel.rs:456` `Channels::runner_for` holds a tokio
mutex across a peer dial:

```rust
let mut runners = self.runners.lock().await;
…
let spawned = crate::remote::pick::runner(&endpoint, endpoint.answer_timeout()).await?;
```

`pick::runner` is bounded per step, not per call: `ONE_STEP = 300 s`
(`remote/pick.rs:257`) for the relay test, `peer_home`, `mkdir`, each of the three
`scp` copies and the `mv`; `DEFAULT_ANSWER_TIMEOUT = 30 s` per greeting. Worst
case is ~46 minutes **while the mutex is held**, and every other remote-bound call
of every session queues behind it with no timeout of its own.

Local sessions are unaffected, which is why the rest of the server looked fine —
and why `fix` hung too: it was listing *another session's* remote channels.

`liche-vm`'s runner connection had died (only the reader's `ssh` was left, idle),
so every call re-entered the dial path and paid that cost inside the lock.

## Reproduction (isolated home, `Endpoint::Direct`, no ssh)

A subagent reproduced the serialization with a peer that never greets
(`notepad.exe` as the runner), which makes `pick::runner` spend the whole 30 s
answer timeout inside the lock:

| call | server `took=` |
| --- | --- |
| `ls_channel` local session (control) | 8.0 ms |
| `ls_channel` remote session, no dial in flight | 8.6 ms |
| `ls_channel` remote session *while another remote call dials* | **25.9 s** |
| `ls_channel` local session during that dial | 8.0 ms |

Scripts: `.dev/repro-hang.sh`, `.dev/repro-lock2.sh` (removed after the run).

## Not established

Why `lichen-vm` and `lichen-vm/templates` never returned at all, while `fix`
returned once its cache-expiry cancel fired. `interrupt_session` on the two was
accepted (10ms) and changed nothing: no `why="interrupt"`, no `tool end`, no
`turn end`. So they are parked outside every place a cancel is read, and outside
`waited`'s abort — which is what the mechanism below leaves as the one gap.

## What was changed (`7ce53992`)

- `Channels::runner_for` dials outside the runners table: a per-machine `dialing`
  mutex is held for the dial, the table only for a look and for the insert.
- `read_from` and `Reader::read_whole` refuse a chunk that does not move the
  cursor, so a runner that omits `offset` is reported instead of re-read.
- `dispatch` gives every command the internal timeout SPEC `## Server Cmd` now
  states, answering `CALL_TIMEOUT_MS` overruns as a harness bug; `input_channel`,
  `run_skill` and `cancel` keep their own clocks.

## What is still open

- **No end-to-end proof of the lock fix.** The stand-in peer (`notepad.exe`) never
  greets, and `set_session remote=<that peer>` dials during placement, so the
  session whose channel would hold the dial cannot be created at all — the run
  that reported `PASS 279ms` had no dial in flight and proves nothing. Proving it
  needs a peer that greets and then answers slowly.
- **`set_session` can spend the whole answer timeout on a peer that will not
  greet** (observed as `gongga-cli: set_session was sent and never answered`),
  which is the same dial, reached from placement rather than from a channel.
- **The running home still serves the old build.** `cargo build` into `target/`
  fails with `os error 5` while the window holds `gongga-tui.exe` and the server
  holds `gongga-server.exe`, so installing this needs the window closed first (or
  a build directory the running pair does not hold).

## The proof attempt, and four things it taught about `Endpoint::Direct`

`.worktrees/fix2/.dev/lock-proof.sh` aims at the assertion the fix is about: two
peers that will not greet, two calls fired at once, wall clock ~one answer timeout
if the dials overlap and ~two if they are serialized. Four runs, no assertion yet:

| run | what it actually measured | why |
| --- | --- | --- |
| 1 | 279ms | no session existed: `ls_channel` on a session with no channel row never asks for a runner |
| 2 | 279ms | sessions refused: the runner path in `remotes.toml` was MSYS `C:/...`-less, so `Command::new` could not find it |
| 3 | 3.5s | sessions and channels fine, but no dial: the kept connections were still alive |
| 4 | 4.1s | the kill matched nothing (`lock-proof` is not in the runner's path), then the live re-read of `remotes.toml` did not produce a slow endpoint within the window |
| 5-11 | 43-99ms | every call finished without a slow dial, including after the peers' runners were killed, after 15s of waiting, and after stopping and restarting the server so the rewritten table could not be missed |

Two findings from those runs stand on their own:

- **`ls_channel` hides a dead peer.** With every runner of the peer killed it still answered
  in 43ms: `list()` turns a failed read into `unread: null`, so a channel nobody can read
  looks like a channel with nothing unread. It is also why it cannot be the probe here --
  it never reaches the dial.
- **A rewritten `remotes.toml` did not change the endpoints of a running server**, not in
  2s and not in 15s, and not after a restart either: a `Direct` peer went on answering in
  70ms while the table named a program that never greets. So the probe never had a slow
  dial to observe, and the lock assertion is still unproven.

### That second finding was wrong, and the correction matters

`.worktrees/fix2/.dev/remotes-probe.sh` and `.dev/switch-probe.sh` re-measured it:

- **both spellings of `GONGGA_REMOTES_CATALOG` work** -- a Windows path and an MSYS
  `/c/...` path both give `set_session remote=p1` an `ok:true`, so "the table was not
  found" is not the explanation;
- **the restart in the two-phase run was real**: phase 1 server pid 57204, phase 2 pid
  71700, same home, same port -- so "the rewrite never reached the server" is not the
  explanation either;
- and yet, with the table naming `C:/Windows/System32/cmd.exe` -- a program that cannot
  speak the runner protocol at all -- `new_channel` still answered `ok:true` and started
  a bash in **73ms**.

So a call that should have dialled did not need to, and which of the two ways it took is
not established: a relay started without a greeting, or the channel started on this
machine rather than on the peer. Either way **none of the eleven runs exercised a dial**,
which is why every one of them measured tens of milliseconds. `Endpoint::Direct` returns
early from `pick::runner` and never runs the `relays()`/`place()` steps, so it cannot
produce the slow dial the old build was measured stalling in (25.9s).

### The `Remote` vehicle: right, and still not measured

`.dev/remote-proof.sh` moved to two `Remote` peers over ssh. What that settled:

- a dial really happens there -- the server logs `pick` and the call takes 1.7-2.8s, where
  `Direct` answered in 70ms;
- a `Remote` needs a **non-empty** `runner`: the reader calls `runner_command()` first and
  refuses an empty one ("this target names an empty runner command");
- a configured runner is only *used* if `relays()` believes a server sits beside it. With
  a bare first word (no path) `relays()` answers yes without looking, and `greet()` then
  runs that word over ssh -- which is how to get a greeting that never comes. My runs
  still logged "the configured runner has no server beside it", so the server that
  answered them was not the one reading the table I had just written: the phase-2
  `server.lock` was missing entirely and the probe went on anyway.
- the harness now refuses to proceed when a server answers but wrote no lock. That, and
  not the lock itself, is what is left to run.


What a peer needs, learned the hard way:

- a `Direct` remote is `[remotes.<name>] runner = "C:/.../gongga-runner.exe"` with
  **no `host`**, and the path must be Windows-spelled: an MSYS `/c/...` path is not
  something `Command::new` can start.
- `gongga-relay.exe` must sit **beside** that runner, or `new_channel` is refused
  with "no relay beside ...". A build of `-p gongga-server` alone does not put one
  there.
- the channel's own `status` file is what `evolve launch` probes, and a channel
  whose program exits at once (`bash -lc "sleep 2"`) is a deaf candidate.

And one isolation bug of mine: the test server was given `--home` but inherited the
session's exported `GONGGA_HOME`, so it put two channel directories in **the
operator's home** and started their relays there. `--home` is not enough; an
isolated run must `env -u GONGGA_HOME` as well. Both directories had no row naming
them and were removed, and both relays were ended by pid after their command line
confirmed they were this worktree's build.


## Consequence for SPEC

`## Server Cmd` now states that **each cmd has an internal timeout that aborts
with a harness bug result**. `ls_channel` has none: its remote branch can stall
for tens of minutes without the command ever ending. Two other unbounded shapes
on the same path are worth the same look:

- `tools/channel.rs:1219` `read_from`'s remote loop has no progress assertion; it
  ends on `eof` or on an empty chunk, and `transport.rs:602` falls back to the
  same offset when the peer omits `offset` — an older runner that does not answer
  `offset` would re-read the same block forever.
- `remote/files.rs:103` `Reader::runner` may spawn a runner per concurrent caller
  (nothing holds the lock across the spawn), so N callers pay N dials.
