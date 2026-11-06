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

Two jobs are built from that primitive and they are not the same job: a shell's
(`own`), which a child may leave if it asks, and a tree the server armed so its
own death reaches it (`own_closed`), which nothing leaves.

Only Windows needs this. On unix the shell is spawned into its own process
group and `kill -TERM -<pid>` already means the tree.

## the limit flags

Two flags, and the pair is the whole point: `KILL_ON_JOB_CLOSE` ends the tree when
the handle closes, and `BREAKAWAY_OK` lets a child ask to leave the job. Without
the second, a process that must outlive its shell could not survive at all.

`BREAKAWAY_OK` rather than `SILENT_BREAKAWAY_OK`: `SILENT` puts every child
outside the job whether it asks or not, and a session that cannot take its own
processes with it breaks a promise this server makes.

## ProcessJob::own / own_closed

The second one has no `BREAKAWAY_OK`, and that is not a detail. A cygwin program
-- the bash this server runs -- forks with `CREATE_BREAKAWAY_FROM_JOB`, so a job
that permits leaving holds the leader and not the tree. Measured: a detached
`bash -c "sleep 12; ..."` in a `BREAKAWAY_OK` job lost its `sleep` when the job
closed, and the `sleep` finished the work anyway. So a shell's job keeps the flag
(that is what a session's shell needs), and an armed tree does not.

## ProcessJob::create

One place the handle is made and the flags are set, so "which flags" is a
parameter of the only constructor and not a second code path.

## ProcessJob::assign

Membership follows the process, not the pid: whoever calls this without proof
that the pid still names the process a row recorded is handing somebody else's
process to a job that kills on close.

## ProcessJob::adopt / adopt_closed

`None` means the job could not be created or the process could not be adopted
-- the caller falls back to killing the pid, which is what it did before this
existed. `adopt` is the shell's, `adopt_closed` is the armed tree's.

## ProcessJob::release

Consuming, and it closes the handle itself. A job whose limits were cleared but
whose handle stayed open would keep its members in a job nothing can name, and
the next `arm` of the same pid would be a second handle on the same tree.
Clearing the flags is not best-effort bookkeeping: if it fails it is said, and
the close still happens, because the members are out there either way.

## ProcessJob

Dropping it terminates the shell and every process the shell started, which is
the behaviour we want in every case: a shell is either in use, or being thrown
away.

## ProcessJob (the `Send` and `Sync` impls)

The handle is a plain kernel handle: no thread affinity, no aliasing rules
beyond "close it exactly once", which the `Drop` below guarantees.

## ProcessJob::drop

`KILL_ON_JOB_CLOSE` is what makes this a cleanup rather than a leak: closing the
last handle to the job ends everything in it. A released job left its handle
null, so the drop of one is a no-op.

## ARMED

Every tree this server holds, by the pid it was armed for, because there is
exactly one act that lets a process outlive the server -- `cancel` -- and it has
to reach every kind of detached job: a detached bash, a worker running one call.
No single owner could hold the handle itself, or a handler that dropped would be
the thing deciding.

What the server is merely *running* is here too, marked, and `cancel` does not
let those go.

## Ends

The field that says which of the two a tree is. `OnCancel` is a detached job: it
was started precisely so a `cancel` would spare it. `WithServer` is a call the
server is answering -- a skill's Python -- which nothing spares, because a stop
that spared it would answer a caller that is no longer waiting.

## arm / hold_served

Both arm, and they differ in one word of the record. False means no job object
could be had, and the caller says so: the process then dies with the server only
when the server lives to end it.

## hold (the replace)

The same pid held twice is one tree under two handles, so the older one is *let
go*, not closed: arming again must never be a way to kill what it guards.

## release

A pid nobody holds is not an error: a tree is released when it ends, and a
release asked twice says the same thing twice.

## release_all

The whole of what `cancel` means for what was detached. Every other exit -- a
kill, a crash, a stop a signal asked for -- calls nothing, so the armed jobs
close with the process and their trees go with it.

## disarm

What a caller's stop means: end the tree now, forget it. False means there was
no job object -- never armed, let go, or already ended -- and the caller's own
kill is all there is. The two are not alternatives: the caller runs both, because
a tree that ended already is still worth the walk, and one that is armed is
better ended exactly.
