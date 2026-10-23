# server/session.rs

## session.rs

Sessions: a row in SQLite plus the named shells that belong to it.

The SPEC keys a shell by `session_name/bash_name`, so a session owns a *set* of
persistent shells rather than one. `default` always exists; anything else is
created on first use and removed by the `del_bash` tool.

## Tuning

Kept beside the session rather than in the agent because it is per-session state:
two sessions on one server can run different models at different compaction
points, and a turn reads this when it starts so a change applies to the next turn
instead of restarting anything.

## Tuning::compact_threshold

`None` is the default percentage, not "off" and not a token count:/n`compaction_budget` multiplies the percentage by the window the chosen model
publishes, defaulting to `DEFAULT_COMPACT_PERCENT` when the session set none.
When no window is known for the model the percentage has nothing to be a
percentage of, so the configured `compress_above_tokens` is used and a warning is
logged. `0` is the only value that turns compaction off.

## Tuning::thinking_effort

`None` means the model's own default -- which is not the same as "send nothing"
for every provider, so it is stored as an absence rather than a value.

## SessionInner::cancels

A turn compares this before and after waiting its turn: if it moved, an
`interrupt_session` arrived while the turn was still queued, and running it
anyway would look like the cancel did nothing.

## SessionInner::gate

Two turns used to run side by side: both measured the transcript, both decided it
was over budget, and both compressed the same thing -- twice the cost for one
summary. A turn and `del_session` had the same problem from a different angle.
Tokio's mutex is first-in-first-out, so this is a queue rather than a
free-for-all: whoever asks first goes first.

## SessionInner::tuning

Behind a lock rather than atomics because it is read as a pair and written as a
pair: a turn must not see a new model paired with the old budget.

## SessionInner::deleted

A turn that was queued behind a `del_session` finds this when its turn comes and
gives up instead of writing to a session that no longer exists.

## SessionInner::resumable

`interrupt_session` leaves this false, because there "stop" means "drop it".

## SessionHandle::close_shells

A persistent shell is a child process that never exits by itself, so leaving one
alive leaves its reaper thread alive too, and the process cannot finish shutting
down. Nothing is lost by ending them: a shell is cheap to start again, and the
transcript -- where the real state lives -- is already written.

## SessionHandle::busy

The answer can change immediately after it is asked.

## SessionHandle::claim

Turns, `del_session` and anything else that reads the transcript and writes back
a different version of it have to take this first.

## SessionHandle::new_turn

This cancels the running turn, so calling it from a turn that has not been let in
yet would stop the one it is supposed to be waiting behind.

## SessionHandle::cancel

The turn is dropped, not deferred: whoever cleans up after it clears its pending
row, so the next start does not owe it anything.

## SessionHandle::cancel_and_preserve

This is what `cancel` (the command) does: it stops work to shut down, not to
throw the work away.

## SessionHandle::cwd

`std::sync::MutexGuard` is not `Send`, so it has to be gone before the first
await. Scoping the lock to a block is what guarantees that.

## ShellSet

Locking is `std::sync` because every operation here is synchronous: spawning is a
process launch and removing is a map delete. The per-shell lock the tools
actually await lives inside, one level down.

## ShellSet::get

Spawning is what makes `del_bash` meaningful: a shell that was removed comes back
fresh (in the session directory) the next time it is named.

## ShellSet::names

`list_bash` reports live shells only.

## ShellSet::retire

This is what happens when a command outlives its deadline: the shell keeps the
command, the session gets a fresh shell under the same name, and the model can go
on working instead of finding a shell that is still busy. The pointer comparison
matters -- if the entry has already been replaced, removing it would take away
somebody else's shell.

## ShellSet::replace

Replacing rather than merely removing is what keeps `check_bash` and `list_bash`
honest: the name has to keep meaning "a shell you can use", otherwise the next
tool call reports that the shell the model was just told about does not exist.

## SessionManager::detached

They outlive shells, sessions and the server. Session-scoped tools still need to
reach them, so they live here rather than inside a shell.

## SessionManager::close_shells

See [`SessionHandle::close_shells`] for why the process cannot exit without this.

## SessionManager::ensure

A session is not "whatever the server's default is today": its reasoning items
carry blobs only the producing provider can replay, so a later change of default
must not silently move it.

## SessionManager::peek

`del_session` needs this: it only has to wait for a session that has work in
flight, and materialising one in order to delete it would start shells for
nothing.

## SessionManager::materialise

`llm` is what the session was pinned to; a row that predates it still knows its
model, and the roster is what turns that name back into a provider. Only a model
the roster has never heard of falls back to the server's own.

## SessionManager::delete

Say it before the rows go, so a turn already waiting behind this deletion sees it
and stops instead of writing into the void.

## SessionManager::live_sessions

Used by `cancel`, which has to stop turns that are actually running rather than
every row in the database.

## cancelling_for_shutdown_preserves_one_turn

`cancel` shuts the server down so the turn can come back, and it says so exactly
once: the next turn is not preserved by accident.

## the_second_claim_waits_for_the_first

What queuing means: the second caller does not get in until the first one is
done. Two turns on one session used to run at once and each compressed the same
transcript.

## deletion_is_visible_to_a_queued_turn

A turn queued behind a `del_session` has to notice the session is gone when its
turn comes, instead of writing into a deleted session.
