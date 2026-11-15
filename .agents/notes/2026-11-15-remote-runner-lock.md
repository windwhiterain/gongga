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
returned once its cache-expiry cancel fired. Two candidates remain: the abort
(`tools/mod.rs:809` `task.abort(); task.await`) landing on a task parked in a
`std::sync::Mutex`, or the dial path simply being re-entered forever because the
peer's runner never comes back. Neither was separable without a stack dump.

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
