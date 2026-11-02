# A launcher the server hangs from is not a waiter

Owed to the maintainer's question, 2026-11-02: *is this the evolve skill's problem or
yours?* Both, and the split is worth writing down because the first answer I gave was
wrong.

## Mine

I ran `taskkill //PID <detached bash -c> //T //F` on the wrapper that had started
`evolve-…-launch.sh` while a handover was in flight. `/T` took the tree, and with it
the launcher that was about to take the home, so the home came up empty until the
maintainer started a server by hand. `AGENTS.md` says exactly this, in the section on
killing processes: `/T` takes the whole tree, and think about what is in it first.

Two things made it easy to get wrong, and both are fixed here:

- `clean` printed `waiters: 0 killed, 0 left running` while *keeping* a launcher, so
  the line read as "nothing is running". It now counts what it kept.
- `status` after the swap said `pid … waiting (current)` for a launcher whose whole
  loop had already ended: the label is decided by pid files, not by what the process
  is doing.

## Not mine: what the measurement changed

I claimed `clean` would have done the same thing. It would not, and finding out took
three probes:

1. `exec` of a native exe on this Windows leaves the launcher bash **dead**. Its pid
   survives only as the server's `ParentProcessId`, so the loop in `clean` — which
   skips a pid `tasklist` does not know — never reaches it. Nothing to kill.
2. MSYS's `ParentProcessId` values are not the Windows pids of the visible bash
   processes (a three-level chain reported ppids of processes already gone), so an
   *ancestor walk* cannot be answered here at all.
3. What a handover leaves is: a dead launcher pid, then the **bundle** (native, alive,
   spawned by the exec'd exe and not recorded anywhere), then the extracted server in
   `payload/<hash>/`, which is what `server.lock` names.

So the honest guard is the one now in the script: `p == server_pid` (unix `exec`), or
`parent_of server_pid == p` (a launcher that survived, and the bundle shape). Those are
the two facts this platform answers; the comment says so instead of claiming a tree
walk it cannot do.

## The test

`scripts/evolve_selftest.sh`, one verdict line, ~24s, every process a stand-in whose
pid is read from the process table by name plus a marker (never `$!`):

- a dead launcher is no candidate, and the server it started is untouched;
- a launcher that holds the server's pid is kept;
- a live parent above the server is kept — **the case the old guard would kill**, and
  the mutation that drops that branch turns this test red;
- a superseded waiter is ended, and the server survives it;
- the summary counts what it kept.

`GONGGA_EVOLVE_RUN` exists for it: the run directory is otherwise hard-coded to
`<repo>/.dev`, and a self-test that drives `clean` must not share the real pointer.
It is also what lets two worktrees on one machine keep their own handover state.
