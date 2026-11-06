# A detached job dies with the server, unless `cancel` let it go

The user's verdict, 2026-11-06:

> server/TUI 被强制关闭，要杀死全部的 job，包括 detached，cancel 才放生 detached
>
> 1. 只有调用 cancel 的路径才放生
> 3. 杀服务端就是连 watchdog 和 server 一起杀

## What was true before

A detached process was *not* owned. It was spawned with `kill_on_drop(false)`, in a
process group of its own, and it asked Windows to break it out of whatever job
object the server was in -- precisely so the server's death would not reach it.
`cancel` and a `taskkill` ended in the same place: the jobs lived.

The window's own path made it worse: the server follows `--parent`, and the parent's
death ran `commands::stop`, which was `cancel`. So force-closing the TUI spared every
detached job, deliberately, and that was written down as an invariant of the module.

## The rule now

The server holds every process it starts in a job object with `KILL_ON_JOB_CLOSE`:
shells, detached bash, detached tool workers, skill scripts, remote runners. Its death
is their death, and that is the operating system's job rather than a shutdown path's --
a kill runs no code.

One act lets a detached job out: `cancel`, asked by a client. It clears each armed job's
kill-on-close and closes the handle, so the processes survive and the next start adopts
the same pids. A stop a signal asked for does *not* -- the window closed, the console was
interrupted, the followed process ended: nothing promised those processes a life after
this server's.

Everything else the server is running is armed as `WithServer`: `cancel` does not spare
it, because a stop that spared a script would answer a caller that is no longer waiting.

## Two things that had to be measured

- A job object with `BREAKAWAY_OK` holds the leader and not the tree: cygwin's bash
  forks with `CREATE_BREAKAWAY_FROM_JOB`, so `bash -c "sleep 12; ..."` left its `sleep`
  outside the job and the work finished after the server was gone. Armed trees are
  created without the flag; a shell's job keeps it.
- Arming has to happen before the first fork, so a detached process is created
  suspended, armed, then resumed. A resume that fails kills it rather than leaving a
  process nothing can reach.

Adoption re-arms, and only on proof: `process_started_at` already gave every row a
process identity, and a job object kills on close, so a recycled pid would make this
server's death somebody else's.

## Proven, not argued

`.dev/dies_selftest.sh` in the worktree, temp homes, pids from `server.lock` and from
the rows, real kills -- `PASS · dies · 18 scenario(s) · 52s · 4debc55`:

- killed outright (`taskkill /F`, no `/T`): the detached bash dies, and the work it had
  started never finishes;
- a detached tool worker dies the same way;
- `cancel`: the job survives, the work it forked inside the armed job still runs, the
  next start adopts the same pid, `del_bash` ends it;
- the followed process killed: the server stops as a signal asks and takes the job with
  it.

## Not done

- SPEC.md still says "detached job lives across server restart". Editing it needs the
  user's permission; the two lines to change are quoted in the report.
- unix has no job object, so a killed server there still leaks its detached jobs. The
  note in `job.rs` says so; no mechanism was invented for it here.
