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
both numbers, and `detached` -- because the thing the caller wanted (run it, do not
hold the turn) is exactly what `detached` is for since SPEC gave it to every tool.

A tool's own configured default is clamped by the ceiling too. It is a config value
written by an operator, and an operator setting `[tools.bash] timeout_ms = 300000`
should not silently outrank the ceiling the same file sets: the smaller of the two
wins, and the refusal above is only about what a *call* states.

## tunnel::wait_of

The resolved number, read back out of the call. A tool that ran on its built-in
duration instead would be a second answer to "how long may this wait", and the second
answer is the one that exceeds the ceiling.

## tunnel::wants_detached and Carried

`detached` reads the same on every tool and means the same thing to the model. What
carries it differs, and only in one way: a tool whose work *is* a process keeps the
registry that outlives the server (`Carried::Process`, which is local `bash` and
nothing else), and every other tool runs its call as a job of this server
(`Carried::Call`). The model is told neither: both answer "started, you will be told".

## tunnel::detach

The job takes its own cancellation token and gives up the turn's stream. Both are the
same fact: the turn that asked for this call ends long before the job does, so a token
it hands down would cancel the work it just asked to continue, and a stream it hands
down would be a channel nobody is reading. What the job keeps is everything else --
the session's shells, its interpreters, its store -- because that is where the work is.

The row is written before the task starts and deleted in the same act that posts the
report. That is what makes a row left behind mean exactly one thing: a process that
stopped never said how this ended. `announce_lost` reads those rows at the next start,
which is the only way a session that was waiting on one learns to stop waiting.

The `max_output` cap is applied here as well as on the waited path, for the reason the
cap exists at all: the answer arrives as a message, and a message with a megabyte of
build log in it is a step that costs more than the build did.

## tunnel::report

Suppressed when the session is gone and when the server is closing, the same two rules
`background_notifier` follows and for the same reasons: a deleted session owes nothing,
and a shell ended by the exit did not finish. The notice is an `inject` row marked
`detached`, so an idle session is not woken for it -- it is read at the next step
boundary, which is what a notice is.

## tunnel::announce_lost

A call carried by the server has no pid, so nothing can adopt it: the row that survives
a start is a job that died with the process. There is nothing to recover, and saying so
is not optional -- the alternative is a session whose `wait` never returns because the
news it waits for was lost with the process that owed it.
