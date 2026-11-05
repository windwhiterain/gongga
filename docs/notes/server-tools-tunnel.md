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

A call that asks for more than the ceiling is refused rather than shortened. Shortening
is the cheaper-looking choice and the worse one: the caller asked for five minutes,
would get one, and would read the result as "it finished". The refusal names the tool,
both numbers, and both ways of not waiting -- `background` for work that is collected
with `check_job`, `detached` for an answer that comes to the session by itself -- or
says to come back later, which is the only honest answer for a reading call.

A tool's own configured default is clamped by the ceiling too. It is a config value
written by an operator, and an operator setting `[tools.bash] timeout_ms = 300000`
should not silently outrank the ceiling the same file sets: the smaller of the two
wins, and the refusal above is only about what a *call* states.

## tunnel::wait_of

The resolved number, read back out of the call. A tool that ran on its built-in
duration instead would be a second answer to "how long may this wait", and the second
answer is the one that exceeds the ceiling.

Three tools read it as a clock of their own rather than as a ceiling: `check_bash`,
`check_python` and `check_job`. They state `default_timeout` as zero -- a read with no
number reads now -- and a number they are given is how long the read waits for the work
to end, not how long it may hold the turn. `wait_ms` was a second name for that same
number, which is exactly the second vocabulary `Tool::default_timeout` exists to prevent;
the ceiling is now the only way a caller says "wait this long".

## tunnel::wants_background, wants_detached, both_ways and Carried

`background` and `detached` both mean "do not hold the turn", and they differ in one
thing only: who reads the answer. `background` leaves it in the job's row for
`check_job`; `detached` hands it to the session as a notice when the job ends. A call
that says both has asked for two answers, so it is refused with one sentence, once,
here -- `bash` used to say it itself.

What carries a `detached` call differs, and only in one way: a tool whose work *is* a
process keeps the registry that outlives the server (`Carried::Process`, which is local
`bash` and nothing else), and every other tool runs its call as a job of this server
(`Carried::Call`). `background` has the matching question, `Tool::handles_background`:
`bash` and `python` answer it themselves through their shell's and their interpreter's
own collector, and every other tool's background is a job of this server.

## tunnel::foreign

The tunnel's words are the tunnel's to offer. `wait` is the one tool it leaves alone,
so `background` and `detached` are refused there by name instead of being silently
ignored: a job that waits is a job that never has an answer to leave running.

## tunnel::spawn

One spawn for both ways. The job takes its own cancellation token, its own inbox and
gives up the turn's stream. All three are the same fact: the turn that asked for this
call ends long before the job does, so a token it hands down would cancel the work it
just asked to continue, a stream it hands down would be a channel nobody is reading,
and the session's own inbox would end the job as if the job had been the turn. That
last one was a real bug for `detached` before `background` arrived: a message to the
session cut the work it was told would keep running.

A call that cannot be recorded is not started. The row is what names the job to
`check_job` and what the next start reads, so accepting a call whose row failed would
be starting work whose answer nobody could ever collect.

The `max_output` cap is applied here as well as on the waited path, for the reason the
cap exists at all: the answer arrives as a row or a message, and a message with a
megabyte of build log in it is a step that costs more than the build did.

## tunnel::report

The row and the notice are written in one transaction. That is what makes a row left
`running` mean exactly one thing -- a job that never got to say how it ended -- and it
is what stops the other two orderings from existing: an answer nobody can find, and a
notice about an answer nobody wrote.

Suppressed when the session is gone and when the server is closing, the same two rules
`background_notifier` follows and for the same reasons: a deleted session owes nothing,
and a job the exit ended did not finish. A `detached` answer is the notice itself, so
it counts as collected the moment that notice is queued; a `background` answer stays
uncollected until `check_job` reads it.

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
