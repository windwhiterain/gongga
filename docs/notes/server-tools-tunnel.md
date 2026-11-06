# `crates/server/src/tools/tunnel.rs`

## tunnel (module doc)

Every tool call arrives with arguments the tool itself does not read: how long it
may take, whether to wait for it at all. Reading them in each tool was the shape the
first version had, and it made the ceiling a per-tool opinion -- `bash` capped at one
number, `skill` at another, and a tool that forgot to look waited for ever. SPEC puts
them on the tunnel, so they are read once, here, and a tool that states its own
default states it as a default (`Tool::default_timeout`), never as a ceiling.

## tunnel::resolve

`timeout_ms` is resolved after the schema check rather than before it, because the
number written back is not a word the call was allowed to say: the check is about what
the model sent, and the injection is about what the tool will read. Both orderings
compile; only this one keeps `argument_error` from reporting a field the model never
named.

A call that asks for more than the ceiling is held to it, not refused. SPEC gives the
number two jobs -- how long the turn is held, and the ceiling no call may exceed -- and
neither is a reason to say no: past its own clock the work becomes a `background` job,
which is a thing the caller can collect, not a failure. Refusing was the first version's
answer, and it read as "you may not run that", which was never true.

The ceiling still binds a tool's own configured default, for the reason it always did:
it is a config value written by an operator, and an operator setting
`[tools.bash] timeout_ms = 300000` should not silently outrank the ceiling the same file
sets.

A tool's own configured default is clamped by the ceiling too. It is a config value
written by an operator, and an operator setting `[tools.bash] timeout_ms = 300000`
should not silently outrank the ceiling the same file sets: the smaller of the two
wins, and the refusal above is only about what a *call* states.

## tunnel::wait_of

The resolved number, read back out of the call. A tool that ran on its built-in
duration instead would be a second answer to "how long may this wait", and the second
answer is the one that exceeds the ceiling.

It is the clock `waited` reads: the moment the turn stops waiting for this call and the
work becomes a `background` job. A tool that says `owns_its_wait` never gets that far --
a shell, an interpreter and a reading tool each answer for their own wait, and a clock
taking the call away from them would be taking away the thing that was asked for.

## tunnel::wants_background and Carried

`background` is the one "do not hold the turn" switch, and its answer waits in the
job's row for `check_job` whichever way the call is carried. There is no second word
to ask for a second answer, so nothing here has to refuse a call that asked for two.

What carries a `background` call is the question SPEC made concrete: a job that lives
across a server cancel has to be a process of its own. `Carried::Process` is local
`bash`, which starts one; `Carried::Worker` is a one-shot call run by the server's own
binary in a worker process (`worker.rs`), which outlives the server and writes its
answer to a file. `Carried::Here` is every tool that acts on what this server holds --
its shells, its interpreters, its jobs -- and a separate process would find none of it,
so `background` is refused there by name rather than answered with a job that would die
with the server. `Tool::handles_background` is the one exception: a shell or an
interpreter keeps its own background jobs, read back by `check_bash` and `check_python`.

## tunnel::foreign

The tunnel's words are the tunnel's to offer. `wait` is the one tool it leaves alone,
so `background` is refused there by name instead of being silently
ignored: a job that waits is a job that never has an answer to leave running.

## No job of this server

There is no `tunnel::spawn` any more. An in-process task outlived the call but not
the server: on a cancel its row survived with no pid to adopt, and `announce_owed`
could only report it lost. SPEC says a `background` job is a process of its own, so
the call now either starts one or is refused. `spawn_worker` keeps the two rules this
path taught: the row is written before the process starts, because a call nothing
records is one whose answer nobody could collect, and `max_output` is capped on the
answer as well as on the waited path, because the answer arrives as a row.

## tunnel::JobRegistry

What `del_job` reaches. A job is stopped through the token that was already ending it:
`waited` in that job watches it, so the cancel is a cut -- the work is aborted, the tool
says what it left behind, and the child process dies with the future. The id is
registered before the receipt leaves and forgotten when the work ends, so the map holds
running jobs and nothing else; a job of a run that stopped has no token left to cancel,
and `del_job` answers for it out of its row.

## tunnel::report

The look that ends a call in explore mode is taken here too, because every asynchronous
ending passes through this function: `background`, a foreground call the
clock promoted, and a job adopted after a restart. See `docs/notes/server-explore.md`.


The row and the notice are written in one transaction. That is what makes a row left
`running` mean exactly one thing -- a job that never got to say how it ended -- and it
is what stops the other two orderings from existing: an answer nobody can find, and a
notice about an answer nobody wrote.

Suppressed when the session is gone and when the server is closing, the same two rules
`background_notifier` follows and for the same reasons: a deleted session owes nothing,
and a job the exit ended did not finish. Every answer stays uncollected in the row until
`check_job` reads it, and the notice only points at that row.

## tunnel::announce_owed

A job carried by the server has no pid, so nothing can adopt it: the row that survives
a start is a job that died with the process. There is nothing to recover, and saying so
is not optional -- the alternative is a session whose `wait` never returns because the
news it waits for was lost with the process that owed it.

The sweep speaks for two kinds of row, and it is the row that decides which: `running`
is a job that will never answer, so it is marked `lost` and said so; `ok` or `error`
with `collected = 0` is an answer nobody read, so the session is told it is still
there. Both are posted after `resume_pending`, which drops the notices the stopped run
had queued -- so exactly one voice speaks for each job, whichever half of the write it
died in. `check_job` reads the same row, which is why the notice can only point at it.
