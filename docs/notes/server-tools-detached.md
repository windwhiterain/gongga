# `crates/server/src/tools/detached.rs`

## detached (module doc)

`background` on a local `bash` is the strongest of the ways it can run, and it
is defined by what does *not* happen to it:

* the call returns at once, so the turn is never blocked;
* the command runs in a process of its own, writing to its log;
* it outlives the shell it was started from, the session, and a `cancel`.

That last word is the whole of it. A background job is how an agent restarts the
server it is running on: launch the replacement in the background, then `cancel`.
`cancel` asked over the protocol is therefore the one stop that lets a background
job go -- it clears the job object's kill-on-close and closes it, so the next start
adopts the same pid.

Every other stop takes them. A signal, a console interrupt, a window that died and
a server killed outright all end every background tree: `stop_all` terminates the
armed job objects and then stops the pids this registry holds, so a tree that
failed to arm is covered too. That is SPEC's "kill server kills all jobs", and it
is an act of the stop rather than a side effect of the process exiting. The rows
are left behind, because a job killed while the server was down is still news for
its session -- `adopt` says so on the next start.

So the process is *owned* after all -- by a job object the server holds -- and
`cancel` is a deliberate release rather than the default. The pid is still
recorded, because a released job has to be findable by the next start: a handle
dies with the process holding it, a pid can be re-adopted, and `adopt` is what
takes the rows left by a previous run and arms them again.

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

## DetachedJob::launch (armed before it runs)

Created suspended, armed, and only then resumed: a job object added after the
first fork would hold the leader and not the tree, which is the failure this is
for. The resume happens whatever the arming said -- a job object that could not
be had is a weaker promise, not a hang -- and a resume that fails kills the
suspended process rather than leaving one nothing can reach.

`BREAKAWAY` is still asked for on Windows: it is the *server's* job that has to
be left, not the one this call just made.

## DetachedJob::stop

Returns true when the process is no longer running, whether because we stopped
it or because it had already finished.

It disarms first: the job object ends every member exactly, however it was
forked, which the tree walk below can only approximate. The walk still runs --
a tree that ended already, or one armed by another run, has no job object here.

## DetachedJob::stop (already gone)

Already gone: nothing to do, and *do not* go looking for its pid. A pid is only
unique until it is recycled, and signalling whatever owns it now would be far
worse than a stale entry.

## DetachedJob::stop (an exited leader)

The handle is what rules out PID reuse, and it only rules it out while the leader
lives. A leader that died while we held it is the "already gone" case, answered
without a lookup of its id at all.

The watcher is what makes this the common case rather than a corner: it calls
`try_wait` on every job it holds, and a job it saw end has its handle spent, so
the id is `None` and the number belongs to whoever took it next. Reporting that
as "not stopped" made the job undeletable -- and with it the session the job
belongs to, since `del_session` keeps a session whose jobs it could not stop.

What the leader left behind is `release_ended`'s answer, not this call's: a
leader that ended takes its job with it, so its descendants were released when
it ended. The `disarm_detached` before it still ends them when this server has
not noticed that end yet -- the job object is then still this call's to end.

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

`cancel` lets detached work go on purpose; deleting the session is the one
place that means "this work is not wanted either", so it ends it.

## DetachedRegistry::adopt

Rows whose process is gone are dropped: that is finished work, and keeping it
would only make the list lie. Rows naming our own pid are dropped too -- that
is a server that recorded its own launch, and adopting it would let `del_bash`
shut this process down. The same goes for any ancestor of ours, which is what a
launcher that spawns the server instead of `exec`ing it leaves behind (see
`is_self_or_ancestor`).

The pid is not taken on trust: `process_started_at` gives a process identity, and a
row whose saved start time and actual one disagree is a recycled pid, not the job. The
tolerance is five seconds, because Unix `ps` reports whole seconds and the row is
written after the fork. A row that cannot be verified is left alone rather than
adopted -- adopting the wrong process would let `del_bash` kill something nobody asked
it to.

A row that passes is armed again (`arm_adopted`): adopting it is a claim that this
server owns it now, and the next kill has to reach it. The identity check comes
first for the same reason it does for `del_bash` -- a job object ends its members
when it closes, and a recycled pid would make this server's death somebody else's.

## arm_adopted

The one place an adopted process is put under a job object, shared with the tool
jobs that adopt their own workers (`docs/notes/server-tools-worker.md`). Only
after the caller proved the pid, which the two call sites check differently: this
one via `same_process`, the worker one via `names_process`.

## names_process

The same question, asked by a caller that holds a row rather than a job: whether
`pid` still names the process that started at `started_at`. `Ok(false)` and an
error are not the same answer, and the worker adoption says which it got.

## release_detached

The whole of what `cancel` means for detached processes: one call, because the
armed trees are one registry. It is the *only* caller of `job::release_all`, and
that is the invariant worth keeping in one place -- a second caller would be a
second definition of "this stop was a cancel".

Uncounted on unix, where there is no job object and nothing to let go of.

## release_ended

A leader that ended takes its job with it, so what it left running is nobody's:
released, not killed. A handover is exactly that case -- the launcher execs a
server and is gone before the old server stops -- and killing there would end the
successor.

## disarm_detached

What a stop asked for by name (`del_bash`, `del_job`, a deleted session, the stop
at exit) uses: end the tree now, forget it. False means there was no job object,
and the caller's own `kill_tree` is what remains.

## DetachedRegistry::adopt (dead rows)

Dead rows are the common case and the cheap one to answer: no walk up the
parent chain for a process that is not there.

They are not dropped in silence. A session that asked for a detached bash and is
waiting for it to end has nothing else to wait for once the process is gone, so the row
says what it can -- the name, the pid, the log -- before it is deleted; otherwise the
wait runs out its ceiling and reports that nothing arrived, which is false. Unless the
row says it was already noticed: one end is one notice, and a notice repeated at the
next start claims a job that finished an hour ago.

## DetachedJob::noticed

Whether the session has been told this job ended, kept in the row as well as in memory.
In memory it stops the watcher posting the same end every half second; in the database
it stops adoption posting it again after a restart. The write happens after the notice,
so a row that says it was noticed has been.

## DetachedRegistry::watch

One pass every `JOB_POLL`, and the pass is exact where it can be: a job this server
spawned has its handle, so `try_wait` is the process, not a guess. Only adopted jobs
need a listing, and they are listed in one batch, which is why the poll rate is a
listing rate and not a per-job signal.

The notice is the row `wait` wakes for. That is the whole reason the watcher exists:
before it, a detached bash's end was visible only to whoever thought to call
`list_bash`, so "stay in this turn until the job you started ends" was a poll loop with
a model in it.

Notices stop with the server: the watcher holds the shutdown token, and a job that
ended because the process stopped did not finish. The `noticed` flag is left unset
there on purpose -- the next start adopts the process and says what it finds.

## kill_tree

A detached command is almost always a shell running several processes, so
killing the leader alone would leave the real work behind. On Windows that
means `taskkill /T`, which only understands real Windows pids -- one more
reason the job has to be our own child rather than a shell background job.

The same reasoning applies to a persistent shell being shut down: its children
hold the write end of the pipe we read, so killing only the shell leaves a
reader waiting for a close that never comes.

## kill_tree (unix)

The escalation is TERM, then a fixed grace window watching the group, then KILL,
and after each of those the group has to be *seen* gone. Delivered signal is
never success -- the kernel accepts the signal and the process may trap or
ignore it -- and an exited leader never implies an empty group, which is why the
answer comes from polling the group and not from the leader's exit status.

## is_self_or_ancestor

`adopt` refuses a row naming our own pid, because `del_bash` kills the whole
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
