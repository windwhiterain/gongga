# `crates/server/src/tools/job.rs`

## job (module doc)

Windows has no process group, so ending "a shell and everything it started" has
to be built out of something. `taskkill /T` looks like that thing and is not:
it walks the tree from the root down, and a process forked between the sweep
and the root's death is left behind as an orphan that still holds the shell's
pipes. That orphan is not cosmetic -- the runtime reads those pipes on blocking
threads, and a read does not return until the last writer closes, so the server
cannot finish shutting down while one survives.

A job object is the primitive that actually means the whole tree. Every process
the shell starts inherits membership, and `KILL_ON_JOB_CLOSE` makes closing the
handle terminate all of them, so there is no tree to walk and no window to
miss.

Only Windows needs this. On unix the shell is spawned into its own process
group and `kill -TERM -<pid>` already means the tree.

## ProcessJob

Dropping it terminates the shell and every process the shell started, which is
the behaviour we want in every case: a shell is either in use, or being thrown
away.

## ProcessJob (the `Send` and `Sync` impls)

The handle is a plain kernel handle: no thread affinity, no aliasing rules
beyond "close it exactly once", which the `Drop` below guarantees.

## ProcessJob::adopt

`None` means the job could not be created or the process could not be adopted
-- the caller falls back to killing the pid, which is what it did before this
existed.

## ProcessJob::drop

`KILL_ON_JOB_CLOSE` is what makes this a cleanup rather than a leak: closing the
last handle to the job ends everything in it.
