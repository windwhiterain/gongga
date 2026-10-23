# `crates/server/src/tools/detached.rs`

## detached (module doc)

`detached` is the strongest of the three ways `bash` can run, and it is defined
by what does *not* happen to it:

* the call returns at once, so the turn is never blocked;
* nothing is collected -- output goes to a log file, not to the model;
* it outlives the shell it was started from, the session, and the server
  process itself.

That last point is the reason this module exists. A detached job is how an
agent restarts the server it is running on: launch the replacement detached,
then `cancel`. Anything the server owned would die with it, so a detached job
is deliberately *not* owned -- it is a process the server knows about, keeps in
the database, and can stop later, including after the server that started it is
gone.

Being killable across restarts is why the pid is recorded rather than the
handle alone: a handle dies with the process holding it, a pid can be
re-adopted. `adopt` is what takes the rows left by a previous run and makes
them manageable again.

## DetachedJob::child

Present only while the server that started it is still running; a job adopted
from the database has a pid and nothing else.

## DetachedJob::launch

The process is put in a group of its own so stopping it takes the whole tree,
and `kill_on_drop(false)` is what makes "detached" true: dropping the handle
must not drag the process down with it.

## DetachedJob::launch (the process group)

Its own process group, so stopping the job can take the whole tree. tokio's
`Command` has these inherently, which is why no `std::os::*::CommandExt` import
is needed.

## DetachedJob::stop

Returns true when the process is no longer running, whether because we stopped
it or because it had already finished.

## DetachedJob::stop (already gone)

Already gone: nothing to do, and *do not* go looking for its pid. A pid is only
unique until it is recycled, and signalling whatever owns it now would be far
worse than a stale entry.

## DetachedJob::stop (a row naming an ancestor)

A row written before `adopt` learned to refuse ancestors, or one put there by
hand. Killing the tree would kill this process, so the honest answer is "I did
not stop it".

## DetachedJob::view

`live` is the answer one process listing gave for every adopted job at once: a
job with no handle left can only be asked about by pid, and asking per job is a
`tasklist` per job -- after a restart, the first `list_bash` walked the list by
starting a process for each entry.

`Some(child)` is exact: the handle is the process. `None` was adopted across a
restart, where only the pid survived, and the listing that answered for the
whole set is already in hand.

## DetachedRegistry

Held by `SessionManager` so `cancel` and `del_session` can reach it, and handed
to `ToolContext` per call so the bash tools can. Everything mutable lives
behind one async mutex: starting, listing and stopping all touch the same map,
and stopping has to await the kill.

## DetachedRegistry::start

The row is written before the job is handed out, so a crash between the two
leaves a recorded process rather than a forgotten one -- and the next start
adopts it.

## DetachedRegistry::list (the listing)

Asked before taking the registry lock: one listing for every adopted job, and
it is a blocking call, so it must not run while the lock is held either.

## DetachedRegistry::stop

Stopped jobs leave the registry either way: what is left is the log file on
disk, which is the record the model asked for.

## DetachedRegistry::stop_session

`cancel` leaves detached work alone on purpose; deleting the session is the one
place that means "this work is not wanted either".

## DetachedRegistry::adopt

Rows whose process is gone are dropped: that is finished work, and keeping it
would only make the list lie. Rows naming our own pid are dropped too -- that
is a server that recorded its own launch, and adopting it would let `kill_bash`
shut this process down. The same goes for any ancestor of ours, which is what a
launcher that spawns the server instead of `exec`ing it leaves behind (see
`is_self_or_ancestor`).

The pid is only checked for liveness, never proven to be the same process, so a
recycled pid could in principle be adopted. The window is narrow and the cost
is bounded by `kill_bash`, which is explicit.

## DetachedRegistry::adopt (dead rows)

Dead rows are the common case and the cheap one to answer: no walk up the
parent chain for a process that is not there.

## kill_tree

A detached command is almost always a shell running several processes, so
killing the leader alone would leave the real work behind. On Windows that
means `taskkill /T`, which only understands real Windows pids -- one more
reason the job has to be our own child rather than a shell background job.

The same reasoning applies to a persistent shell being shut down: its children
hold the write end of the pipe we read, so killing only the shell leaves a
reader waiting for a close that never comes.

## pid_alive

Used for jobs adopted across a restart, where there is no handle to ask and
`try_wait` is not available.

## pid_alive (CSV)

CSV keeps this locale-independent: the fields are always image, pid, session,
session number, memory -- in that order -- so the pid is always the second one
and no header has to be parsed.

## is_self_or_ancestor

`adopt` refuses a row naming our own pid, because `kill_bash` kills the whole
tree and a job that is us would let the model shut the server down. That check
only covers a launcher that `exec`s the new server: `exec` keeps the pid, so
the row ends up naming us. A launcher that *spawns* the server and waits --
`bash --detached "cd . && ./gongga-server"`, which is what a hand-written
launcher looks like -- leaves the row naming our parent, and the tree under
that pid still contains us. Same trap, one level up, which is why the answer is
the whole chain and not just the parent.

## is_self_or_ancestor (the bounded walk)

Bounded because a parent chain is a handful of links and a snapshot can lie: a
pid recycled mid-walk must not turn this into a hang at startup.

## parent_of (windows)

One snapshot per hop rather than one for the whole walk: the walk is a few
links deep and a snapshot is a kernel call -- unlike `tasklist`, which is a
subprocess, and this runs on the startup path where that matters.

## live_batch

The listing it runs is a blocking `Command::output()`: on a loaded machine it
takes long enough that calling it inline stalls everything else this thread was
going to do -- at startup, that is the bind every client is waiting on.

## alive_pids

Adoption used to ask one pid at a time, and each question is a `tasklist`
subprocess: a home that had run a few hundred detached jobs spent over a minute
walking rows of finished work before it ever bound the port, so a client
restarting a server saw only a timeout. Nothing about the question needs to be
asked per pid -- one listing answers every row in it.

A listing that cannot be read answers "none are alive", which is what the
per-pid version did when `tasklist` failed: the rows are then dropped as
finished work rather than adopted on a hunch.

## alive_pids (no `/FI`)

No `/FI`: the point is one call for the whole set. CSV keeps this
locale-independent -- image, pid, session, session number, memory -- so the pid
is always the second field of a row.

## tests::liveness_is_answered_for_many_pids_at_once

One call answers the whole set: our own pid is in it, a pid nobody can be
holding is not, and asking about both costs one listing.

## tests::jobs_from_a_previous_run_are_adopted (closing Stdio)

Stdio is closed on purpose: a child holding the test's stdout pipe open keeps
`cargo test` waiting for output long after the test ends.

## tests::a_server_never_adopts_a_job_that_is_its_own_parent

Our own pid is the `exec` case; the parent is the spawn-and-wait case, which is
what a hand-written launcher produces. Adopting it would put `taskkill /T` one
call away from the server's own process tree.
