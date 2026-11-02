# `crates/server/src/tools/mod.rs`

## tools (module doc)

The SPEC gives the model `bash{name, cmd}` to run a command in a named
persistent shell and `del_bash{name}` to throw one away; `check_bash` and
`list_bash` are the reading half of the same idea, and `del_bash` exists
because a `detached` job outlives everything else and therefore needs an
explicit way to be stopped. Shells are keyed `session_name/bash_name`, so two
sessions never share one and a single session can keep several independent
ones.

Nothing here drives the harness. There was a `harness` tool that called the
dispatch in-process, and a tool call cannot be piped, looped or written into a
script, so the agent's commands are a line it types in `bash` instead -- see
`server-shell-command.md`. `with_builtins` is the whole registry: the four bash verbs, the skill
index; `with_python` adds the persistent interpreter described in
`server-tools-python.md`.

## ToolContext::detached

Deliberately not part of a shell: a detached job outlives its shell, its
session and the server.

## ToolContext::store

Where a command that outlived its deadline reports back to the session when it
finishes. That report is an inbox row, not a tool result: the call it belongs
to returned long ago, and the session may be idle by the time the command
ends.

## ToolContext::replace_shell

Every path that ends a shell for a reason the model did not ask for -- a
command that outlived its wait, output that lost its sentinel, a turn
cancelled mid-command -- does the same thing afterwards: the name has to keep
meaning "a shell you can use", because the tool results and `list_bash` say it
does. The pointer check inside `replace` is what keeps this from taking away a
shell somebody else already put there; `false` means the name now belongs to a
different shell.

## ToolContext::cap

Cap one command's output, spilling the rest to a file named after where it
came from: session, shell, `description`, call id.

The `description` is in there because a dump is read later, out of context -- a dozen
`...-default-call_00_abc.out` files say nothing, and the whole point of
keeping the full output is being able to find the right one again. `slug()` in
`bash.rs` is what makes it safe for a path.

## Tool

Implemented with a boxed future rather than `async_trait` so the crate keeps
one fewer macro dependency; the signature is the same either way.

## argument_error

Assembled from [`Tool::parameters`] rather than written per tool, so it cannot
drift from what the model was handed: which arguments exist, their types, which
are required, and which declared name a wrong one was probably reaching for. A
hint that has to be maintained by hand is a hint that goes stale the first time
an argument is renamed.

Only a *missing required* argument rejects the call. A stray key on its own is
passed through, exactly as it was before this existed: a call that would have
run is a call the operator wanted, and failing it over an ignored key would
manufacture the retries this message exists to prevent.

## is_shorthand_of

`cmd` is inside `command` and `command` is inside `cmdline`, so direction does
not matter: a name the model invents for an argument is nearly always a
shortening or a spelling-out of the declared one, and naming the declared one
back is what turns a rejected call into a corrected one. This is a rule over
the schema, not a list of synonyms, so a renamed argument never needs a new
entry -- and a key unrelated to anything declared gets no guess rather than a
wrong one.

## WaitingMessage

Its own type for the same reason `Cancelled` is: the two say opposite things
about the same shell. A cancelled command is killed; a backgrounded one keeps
running and owes its output later. Told apart by string, that distinction is a
bug waiting for someone to reword a message.

## INBOX_POLL

A tick rather than a notification. Notifying every waiter needs somewhere to
keep them, and the number that matters is not "did one arrive" but "did one
arrive for *this* call", which is a per-call question: the call answered it by
comparing a copy of the count it took when it started. A quarter of a second is
well below anything a reader notices, and the cost of the tick is one load.

## ToolContext::inbox

Where the signal comes from: the session owns the counter, and every tool call
in a turn takes a fresh copy, so a message wakes exactly one wait instead of
every call that follows it.

## Cancelled

Its own type rather than a string, because the cancel path has work to do that
no other error does: the command has to be stopped, and the shell it was
running in has to be thrown away. A message-shaped error would make that work
a string comparison.

## BashTool::run (the `command` argument)

The argument is `command`, not `cmd`: a model reaching for the word it already
knows is right far more often than it is wrong, and the schema should be the
thing that bends. A missing one is rejected by `argument_error` first, in the
schema's own words; what is left here is a `command` that is not a string.

## BashTool::run (the `description` argument)

Two readers, one string: the call's row (SPEC's `toolcall{tool_name·description}`)
and the spill file's name. Only ever a label either way, so a missing or
nonsensical one costs an untitled row and a less informative path rather than a
failed call -- which is why it is read here and not validated.

## BashTool::run (background and detached)

Both flags return at once, which is the point: the turn stays responsive
instead of sitting out a long command. They differ in what happens to the
output, not in whether the call blocks.

## BashTool::run (a detached job's cwd)

Only the working directory is inherited. Exports and shell functions are not,
because a detached job does not run in the shell -- it is a process of its own.

## BashTool::run (relaying output)

Relay shell output to `tool_answer` events while the command runs. Draining has
to be concurrent with the run itself: if we awaited the command first, a
chatty command would fill the channel and block the shell.

## BashTool::run (the scoped future)

Scoped so the future borrowing `shell` is dropped before the guard is, which is
what lets the guard go out of scope at all.

## BashTool::run (a cancelled command)

A cancelled command is stopped, not abandoned. Dropping the future only stops
us reading: the shell keeps the command, and would answer the *next* call with
its leftovers (and with a success status that belongs to it). Killing the
process group is both the "turn cancel interrupts the foreground command"
contract and the only way to know the shell has no command boundary owed.

## BashTool::run (reading `usable` under the guard)

Read while the guard is still held: a shell that lost its output stream must
not be handed another command, and this is the last moment its state is
visible without taking the lock again.

## BashTool::run (still running after the wait)

The deadline was on waiting, not on the command. Nothing was stopped: the shell
still holds it. Give the session a fresh shell under the same name so work can
continue, and have the old one report the rest of the output into the inbox.

## BashTool::run (a message arriving mid-command)

This is the fix for "the session went quiet for five minutes while I was talking
to it". A step is one model request *plus* every tool call it asked for, and the
inbox is read only between steps, so a command that sleeps for its whole
`timeout_ms` -- 300 seconds, the way it was configured that one time -- holds
whatever the operator said until the deadline arrives. The turn was not stuck;
it was in the middle of a step that nobody could shorten.

Ending the wait early does not touch the command. It keeps running in the old
shell, which goes through the same hands as a command that outlived its
deadline, so there is one story for "this call returned without its output" and
not two. What the model hears differs only in the reason, and the reason is the
part worth knowing: nobody told it to stop, somebody wanted to talk to it.

Only this tool waits long enough for it to matter. A `read_image` call is a file
read; nothing is gained by interrupting that.

## background_notifier

The other half of `leave_running`: a command that outlived its wait is noticed the
same way a `background` job is. The row is an `inject`, which is the only shape
that reaches both states a session can be in -- busy reads it at the next step
boundary, idle is woken for it by `commands::nudge_inboxes`. Posting a tool result
instead would only ever reach a turn that had already moved on.

The notice carries how long the job ran, because nothing else in the session can:
`check_bash` returns output and a count, `list_bash` a number of jobs, and the call
that started it returned before the job had a duration to report. `spent` spells it,
so a notice and a tool result write a duration the same way.

A deleted session is skipped rather than written to: nothing would ever read that
row, and its name may be reused by a different session later.

A session whose shells are being closed is skipped for the same reason, and a
sharper one. `close_shells` ends every shell the session owns, retired ones
included, so a shutdown makes each waiting command report "it has now finished"
-- which is false: it was stopped, and the shell that would have held its output
is gone. Writing those rows is what made a restart wake every idle session to
read notices about commands nobody was waiting for any more. The flag is set
before the first shell goes (`SessionHandle::closing_signal`), and the row is the
one thing it suppresses.

## leave_running

Both ways of losing a command's output -- its deadline passed, or somebody spoke
-- leave the same state behind: a fresh shell under a name that has to keep
meaning "a shell you can use", and an old one whose output arrives later. Having
them share one path is what keeps those two promises from drifting apart, and
the messages differ only in `because`, since that is the only thing the model
needs to know that the two paths do not share.

It takes the clock rather than a duration, so both callers measure the same thing
and neither can hand it a number that is not the wait: the deadline arm used to
pass its `timeout_ms`, which is the ceiling it asked for rather than the time it
spent. The result says `still running after 1.0s`, and the notice later says how
long the command ran in all -- two numbers, because a model deciding what to do
next needs the second one, and reporting only the wait would let a command that
ran for a minute read as one that ran for five seconds.

Both are `spent`'s spelling, and the notice is the only place the total exists:
the row that carried `[elapsed ...]` for the call was written when the call
returned, an hour before the command did.

## BashTool::run (the late spill label)

Same naming rule as the foreground path, with `late` standing in for the call
id: the call it belonged to has already returned, so its id is no longer what
identifies the run.

## BashTool::run (the late lock)

Nobody else holds this shell any more, so waiting for the lock is waiting for
the call that timed out to return -- which has already happened.

## BashTool::run (a pipe without its sentinel)

The pipe ended without its sentinel, so this shell cannot tell one command's
output from another's. It is replaced here rather than on the next call, so the
model never gets to send a command to a shell that would answer it with the
previous command's text.

## BashTool::run (capping before storing)

Capped before it is stored: what goes into the transcript is replayed on every
later step, so the full output of a chatty command would be paid for over and
over.

## BashTool::run (a cancelled result)

Same treatment as lost output: the shell may still be holding part of a command
that was stopped mid-flight, so it is never handed another one. The result says
what happened instead of pretending the command finished -- the model must not
read a cancelled command's silence as success, and it must not expect its
output.

## BashTool::run (a failed write)

The write itself failed, which can leave the shell with half a command line in
its stdin. `healthy` is what says so; dropping it here is how the next call
ends up reading this one's leftovers.

## ListBashTool::run (detached rows)

Detached jobs belong to the session, not to a shell, so they are listed on
their own -- including ones adopted from a previous run of the server, whose
shell no longer exists.

## DelBashTool::run (an unanchored pattern)

Unanchored on purpose: `qa` is how someone actually names a shell family
(`qa`, `qa2`, `qa5`), and requiring `^qa` would make the cleanup of a long
session's leftovers guesswork.

## DelBashTool::run (surviving detached jobs)

Detached jobs are not the shells', so they keep running; say so, or the model
would assume discarding the shells stopped them.

## a name is one shell, on one machine

`bash` and `remote_bash` are one implementation driven twice: `BashTool { machine }`.
A session with a remote is offered both; a session without one is not offered the
second, because a tool that can only refuse is a wrong turn nobody can undo.

`check_bash`, `del_bash` and `list_bash` carry no machine: a name is one shell, and
whichever machine holds it is the one that answers. `list_bash` reports it per row and
`check_bash` in its closing bracket, so a reader can tell `peer` from `here` without
asking. A name already taken on the other machine is refused, naming the machine that
has it.

The directory a shell starts in follows the same rule as the session's own: the
machine that owns the placement gets it, and the other side starts in its own default
directory. `default` is therefore the directory-owning machine's shell -- so a remote
session's `default` is the peer's, and `bash` there means *name another shell*.

## `remote_bash` (the `target` argument)

SPEC declares `target = session's remote`, and the argument is what a model writes when
it is following the spec rather than this schema. `argument_error` passes a stray key
through rather than failing the call (see above), so an undeclared `target` was a key
that silently did nothing: the command ran on the session's peer while the model
believed it had chosen a machine. Declared, it is refused unless it names that peer,
and the refusal names `set_session --remote`.

Only one value can be accepted, because the other machine would have no directory to
stand in: the session's cwd is the peer's path, and a reference printed by a `bash`
call on a second peer would be read through the first one's connection.

## the schema is per machine

`BashTool::parameters` builds the two fields only one machine has: `background` and
`detached` on this machine's `bash`, `target` on the peer's `remote_bash`. A flag a
machine cannot honour is not a flag the model should be shown -- and both are still
refused at run time, because the refusal is what explains *why*, and the schema is what
keeps the model from asking.

## the machine a spilled cut belongs to

A capped output is spilled to a file this server can name only when the shell that
printed it runs here; over a remote the path would name a file the reader cannot
open, so the cut is reported without one.

## spill_label

One function rather than two `format!`s because the two paths that spill -- a
foreground result, and the late output of a command that outlived its wait --
must produce names that sort and read the same way. The `task`, when the model
supplied one, is what makes a dump identifiable later; without it the name is
still unique, just less useful.
