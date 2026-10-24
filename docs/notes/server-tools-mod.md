# `crates/server/src/tools/mod.rs`

## tools (module doc)

The SPEC gives the model `bash{name, cmd}` to run a command in a named
persistent shell and `del_bash{name}` to throw one away; `check_bash` and
`list_bash` are the reading half of the same idea, and `kill_bash` exists
because a `detached` job outlives everything else and therefore needs an
explicit way to be stopped. Shells are keyed `session_name/bash_name`, so two
sessions never share one and a single session can keep several independent
ones.

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
came from: session, shell, `task`, call id.

The `task` is in there because a dump is read later, out of context -- a dozen
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

## BashTool::run (the `task` argument)

Only ever a label: it names the spill file and nothing else, so a missing or
nonsensical one costs a less informative path rather than a failed call.

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

## leave_running

Both ways of losing a command's output -- its deadline passed, or somebody spoke
-- leave the same state behind: a fresh shell under a name that has to keep
meaning "a shell you can use", and an old one whose output arrives later. Having
them share one path is what keeps those two promises from drifting apart, and
the messages differ only in `because`, since that is the only thing the model
needs to know that the two paths do not share.

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

## spill_label

One function rather than two `format!`s because the two paths that spill -- a
foreground result, and the late output of a command that outlived its wait --
must produce names that sort and read the same way. The `task`, when the model
supplied one, is what makes a dump identifiable later; without it the name is
still unique, just less useful.

## tests::a_call_missing_a_required_argument_is_answered_with_its_own_schema

A rejected call has to carry enough of the schema for the next attempt to be a
correction rather than another guess -- and it has to be assembled from the
schema, not from a list of names somebody remembered to update.

The argument abbreviated is the shape of the mistake that used to be spelled
out the other way round, hundreds of times per session.

## tests::a_task_names_the_spill_file

`task` must be offered to the model, must not become something the call can
fail on, and must actually reach the file name.

Still only `command`: a label drifting into a required argument would turn a
rejected call into a rejected *idea*, and the model would stop describing what
it is doing.

## tests::a_message_arriving_mid_command_hands_it_to_the_background

A real shell and a real `sleep`, because the thing being tested is that the wait
ends while the command does not: anything mocked out would have passed before
this existed too. It asserts on the elapsed time rather than on how many ticks
happened, which is the number that made this worth writing.

The second call takes a fresh signal, the way a turn does. Reusing the old one
would report the same message again and hand every later command straight to the
background -- the failure mode this whole design exists to avoid.

## tests::detached_jobs_have_an_explicit_way_to_end

Omitting the job number means "every job of that shell", which is the only
shape that makes sense after a restart. `job` is optional for the same reason.
