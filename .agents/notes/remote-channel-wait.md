# WIP: a remote channel's start belongs to the wait — `fix/remote-channel-wait`

Where this stands, and what is still open. The measurements are in `.dev/verify/`
and the design record is `docs/notes/server-channel.md`, "A call never dials".

## Done (worktree `.worktrees/remote-wait`, branch `fix/remote-channel-wait`)

- `Channels::start` on a peer writes the row (`begin_channel`) and spawns the start;
  the call asks the peer nothing. `talk` waits for the start inside the caller's
  `wait`; every other reader uses `place`, which never dials.
- A failed start is `channels.start_error`: the network's words, machine named, never
  an exit status. Read by the call that is inside the window, or by the watcher for a
  call whose wait ran out first; the row goes with the answer.
- The per-machine dial mutex is gone: one `Dial` per peer, joiners wait for its
  outcome. No lock is held across `pick::runner`.
- Gates run green on `94104808`: `mbx build`, `mbx fmt --check`, comments (parity with
  the baseline, 71 = 71), `test-layout-check`, `scripts/skill_contract.sh`,
  `scripts/channel_selftest.sh` PASS, `scripts/channel_remote_selftest.sh` PASS 37 s
  (13 checks, real peer `lichen-vm`).

## Two pre-existing bugs the selftest turned up (fixed here)

- `scripts/channel_remote_selftest.sh` had a literal `\n` where a line continuation
  belonged, so it dialled the host `n` and printed `skip:` on every run — the only
  end-to-end proof of this feature was never running.
- Its preflight asked `ssh <peer> true`, which a peer whose login shell is PowerShell
  cannot answer. It now asks through `bash -lc`, the way the harness itself speaks.

## Open

- **The live home still runs the old build.** Handing over (`evolve`) cancels the
  running server, which ends the four turns parked since 10:51 on `lichen-vm`
  (`Rusd/pcg-slice`, `Rusd/agent-preview`, `Rusd/viewer-ui`, `lichen-vm/impl`) — they
  are stuck on the very path this change replaces. Needs the user's go-ahead.
- `scripts/channel_notice_selftest.sh` fails one check ("the queued notice is read by
  the next turn") **on main as well** (7db8564f) — pre-existing, not caused here.
- `pick::runner` swallows the peer's stderr when placement fails, so a refused
  connection reads as "the peer did not say where its home is". The shape is right
  (network, machine, no exit) but the cause could name the ssh error.
