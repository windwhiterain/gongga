# server/session.rs

## session.rs

Sessions: a row in SQLite plus the named shells that belong to it.

The SPEC keys a shell by `session_name/bash_name`, so a session owns a *set* of
persistent shells rather than one. `default` always exists; anything else is
created on first use and removed by the `del_bash` tool.

## Tuning

## SessionInner::explore

`Some(watch)` is explore mode, `None` is work. The watch is the session's chained
snapshot, so the field carries both halves of the same fact -- whether this session
explores, and what it explores from -- and nothing has to keep two of them in step.

`set_mode` down to `work` drops the snapshot rather than keeping it warm: a snapshot
from an hour ago would report every file the session wrote in between as a change the
moment it went back. Up from `work` starts a fresh chain, which costs one `First` look
and no reminder. See `docs/notes/server-explore.md`.

Kept beside the session rather than in the agent because it is per-session state:
two sessions on one server can run different models at different notice
points, and a turn reads this when it starts so a change applies to the next turn
instead of restarting anything.

`Tuning::model` is the model a turn here spends: `provider/model_name`, or `None` for
"nothing chosen for this session", which [[Agent::route]] refuses rather than filling
in with the server's own. It is one field because a model is one choice -- whose account
pays and which brain answers are not separable, and a struct that could hold a
provider without a model would be a state nothing could send.

## Tuning::context_limit

`None` is the default percentage, not "off" and not a token count:/n`notice_budget` multiplies the percentage by the window the chosen model
publishes, defaulting to `DEFAULT_CONTEXT_LIMIT_PERCENT` when the session set none.
When no window is known for the model the percentage has nothing to be a
percentage of, so the configured `compress_above_tokens` is used and a warning is
logged. `0` is the only value that turns the notice off.

## Tuning::thinking_effort

A level in the roster's spelling, resolved by `set_session` through
`Entry::level_in_force` and stored, so the pane's title and the request read the same
word. `None` is not "the model's own default" any more: it is a model that publishes
no level, which is the one case where nothing goes on the wire. See
`docs/notes/server-commands.md`.

## SessionInner::cancels

A turn compares this before and after waiting its turn: if it moved, an
`interrupt_session` arrived while the turn was still queued, and running it
anyway would look like the cancel did nothing.

## SessionInner::inbox

A count, not a queue, because nobody cares how many messages are waiting -- only
that one arrived while this turn was doing something it could not interrupt. A
tool call takes `inbox_signal` (a copy of the count) when it starts waiting, so
"did one arrive since I began" is one load and no bookkeeping: whatever was
already queued when this call started belongs to some earlier call.

Shared rather than embedded so a call can hold a copy across an await; keeping
it per session rather than per call is what stops every call in a turn from
reacting to the same message again.

## SessionInner::gate

Two turns used to run side by side: both measured the transcript, both decided it
was over budget, and both did the same work -- twice the cost for one result. A turn and `del_session` had the same problem from a different angle.
Tokio's mutex is first-in-first-out, so this is a queue rather than a
free-for-all: whoever asks first goes first.

## SessionInner::tuning

Behind a lock rather than atomics because it is read as a pair and written as a
pair: a turn must not see a new model paired with the old budget.

## SessionInner::deleted

A turn that was queued behind a `del_session` finds this when its turn comes and
gives up instead of writing to a session that no longer exists.

## SessionInner::resumable

Set by a `cancel` that found the turn inside `wait`. `interrupt` reads the mark
before it fires the token, calls `mark_resumable`, and keeps the `turn` row; then
`finish_turn` preserves it and the next start resumes the turn. Every other turn is
interrupted and owes nothing, so this is false for it. `interrupt_session` and
`del_session` never set it -- their "stop" means "drop it".

## SessionInner::waiting

Whether a call is inside `wait` right now. `StopSignal::begin_wait` sets it and its
guard clears it on drop, which an aborted call also runs: a wait that ended for any
reason is not a wait a later cancel can find.

## StopSignal

A copy, not a handle on the session -- a tool must not be able to do anything else to
the session that holds it. It carries `resumable` (what the stop decided) and
`waiting` (whether this call is a wait), and nothing else: the token, not a flag, is
what ends a call.

## SessionHandle::open — the part still being written

The text of an item the model is still streaming, kept per part and dropped the
moment its row is written. It exists for one reader: a window that reconnected
mid-answer. The transcript has no row for what has not finished, and the draft on
disk is flushed every quarter second, so without this the tail of the answer would
be lost for good.

Every update happens inside `EventBus::step`, which is what makes a snapshot exact
with respect to the stream's cursor: the text and the chunk that carries it land in
one step, so a reader that takes the text afterwards also knows every chunk it
already contains. `open_clear` at the end of a turn is the backstop: no part
outlives the turn that wrote it.

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

## SessionHandle::request_stop

It is gone. `cancel` (the command) no longer asks a turn to stop cleanly: it fires
each live session's turn token and clears that session's pending rows instead, so
the work is interrupted where it is and nothing is owed to the next start.

The one exception is a turn inside `wait`. It started nothing, so keeping it cannot
run anything twice: `interrupt` marks it `resumable`, keeps its `turn` row, and the
next start resumes the wait. `interrupt_session` and `del_session` still drop it --
their "stop" means the turn is not wanted.

`cancels` is still bumped, so a turn that is queued and has not started is dropped
instead of running into a shutdown.

## SessionHandle::cwd

It used to take the shell's own lock to read `cwd`, which meant that asking where
a session was waited for whatever command was running in that shell -- `set_session`
answers with this, so creating a session B whose default shell happened to be busy
could hang for minutes in A's `gongga-cli` call. Now it reads the cell the shell
mirrors its directory into, which needs no lock at all.

Staleness is not the cost of a race: the mirror is written by the same statement
that sets `cwd`, from code that already holds the shell's lock, so a reader sees
the last *reported* directory -- the newest value that exists.

## checked_name

SPEC's grammar for a session's name, in one function, called by the one command that
creates a session.

It refuses empty segments and nothing else. `/` is what makes a name a path -- the list
draws one row per prefix, and `a//b` or `a/` has no row to be drawn on -- while every
other character is the operator's: a space, a `.` and a CJK segment draw as themselves.
Refusing them would be this function inventing a policy SPEC does not have.

The prefix that matters is the one the harness owns, and that refusal is
`monitor::reserved`'s, kept separate: it now checks every segment rather than only the
first, because `work/@monitor/error` draws a row a reader cannot tell from the
harness's own session. Two functions, two reasons, both stated where they refuse.

## stated_directory

What `--cwd` means once resolved: absolute, `~` expanded, and a relative path read
from `base`. Both ends of it matter.

The row is what a later start hands to a shell, so a path stored as typed would move
with wherever that shell happened to be; `std::path::absolute` is enough because it is
lexical, and a row may name a directory that has since been removed: it is `set_session`
that refuses a `--cwd` the destination does not have, asked at the moment a caller
states one (`SessionManager::check_placement`), never a start that reads a row.

Refusing at the statement is what keeps a removed directory from costing a session:
reached at the start instead, it would either kill a session that was working or --
worse, for this machine's own shells -- fall back to the server's directory and say
`ok`, so a command ran somewhere the operator never named.

`placed_directory` is the one place the two spellings meet: a path landing here is
made absolute, and a path landing on a peer is passed on exactly as written, because
only that machine can expand it.

`base` is the caller's statement of where the operator was speaking from: the
session's own directory for a move on this machine, the server's for a session being
created, and nothing at all for a peer -- its path is expanded by the machine that
owns it. The completer reads the same base, so a completed `sub/` inserts a word that
means the same directory at both ends. See `docs/notes/cli-complete.md`.

## ShellSet::reported

The cell belongs to whichever shell is currently named `default`, which is what
[`ShellSet::get`] arranges. A replacement therefore brings a new cell rather than
letting the retired shell keep writing into the session's answer: once the name
points at a fresh shell, its directory is the truth, even while the old one is
still finishing the command it was left behind with.

## ShellSet

Locking is `std::sync` because every operation here is synchronous: spawning is a
process launch and removing is a map delete. The per-shell lock the tools
actually await lives inside, one level down.

## ShellSet::get

Spawning is what makes `del_bash` meaningful: a shell that was removed comes back
fresh (in the session directory) the next time it is named. Naming it also hands
the session this shell's `cwd_cell` when the name is `default`, which is how
[`ShellSet::reported`] stays pointed at a live shell.

## ShellSet::reported_cwd

Empty only if the cell was poisoned by a panic someone else already reported.

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

## SessionManager::ensure_on

A session is not "whatever the server's default is today": its reasoning items
carry blobs only the producing provider can replay, so a later change of default
must not silently move it.

## SessionManager::peek

`del_session` needs this: it only has to wait for a session that has work in
flight, and materialising one in order to delete it would start shells for
nothing.

## SessionManager::materialise_locked

The row's `model` is what the session is pinned to, `provider/model_name`. A row that
predates that spelling knows the bare name, and the roster is what turns that name
back into a model; a row the roster has never heard of keeps the name it has, and a
session whose row names no model has none: nothing here fills in the server's own.

## SessionManager::delete

Say it before the rows go, so a turn already waiting behind this deletion sees it
and stops instead of writing into the void.

Then the files. A generated picture is not a row: it is a file beside the home that a
row names, so the delete asks the database which names belong to this session alone and
unlinks those -- and only those, because an `heir` copy leaves a mentor session naming
the same bytes. The order is not arbitrary. Asking first is the only way to know, and
dropping after means a failure in between leaves files whose rows still exist, which is
recoverable, rather than rows whose files are gone, which is not.

## SessionManager::live_sessions

Used by `cancel`, which has to stop turns that are actually running rather than
every row in the database.

## session_vars and SessionHandle::env

A session's shells were the only programs that learned whose they were: `session_env`
built `GONGGA_SESSION`, `GONGGA_MODEL` and `GONGGA_REMOTE` for bash. Everything else the
server started -- an `exec` program, a Python, a skill's entry -- inherited the *server's*
own environment, which belongs to whoever launched it. Measured, in this repository's own
session: the `python` tool answered `$GONGGA_SESSION` with `tui` while running for `arg`.

That is not a missing convenience, it is a wrong answer to "which session is this" -- and
`subagent`'s `spawn` reads exactly that variable to choose a child's parent. So the pair
is now split: `session_vars` is what one session adds (name, model, machine, read from the
rows each time, so a `set_session --model` is what the next child sees), and
`SessionHandle::env` is the base for programs that are not shells -- the same facts with
the native `PATH` a Windows program reads, mirroring `script_env`.

Tools hand this to the children they start and nothing of their own: `ToolContext::env` is
`SessionHandle::env`, and `bash` keeps using the shells it already has.

## What a stop leaves a wait (`StopSignal::wait_until`, `interrupt`, `resume_pending`)

SPEC's `cancel` splits the sessions it interrupts in two. One that is not waiting is
*flagged*: the next start continues the turn. One that is inside `wait` is not
continued at once -- the wait itself is, for the time that was left. So the fact that
a session was waiting is not enough to resume it, and a duration is not either: the
process that reads it has not started yet. What survives is the instant the wait was
due to end, in epoch milliseconds (`sessions.waiting_until`), written by `interrupt`
before the token -- after the token there is no turn left to ask -- and read by the
start that inherits it.

Three other places have to honour that row, or the wait is skipped:

- `deliver` queues a message when the server is stopping instead of starting a turn
  on it: the same stop cuts that turn a moment later, and the row is what the next
  start owes.
- `nudge_inboxes` and `resume_pending`'s pending-row loop both skip a session whose
  deadline is still in the future: starting its turn is the wait, skipped.
- `resume_pending` itself arms a timer per waiting session, sleeps what is left, takes
  the row out (`set_waiting_until(id, None)`) and starts the turn the wait was holding.
  Cleared at the moment it fires rather than when it is read, so the row the wait
  leaves behind is what keeps a nudge away until then.

`cancel` counts the waiting sessions before it interrupts them -- each `interrupt`
clears its own session's mark -- so the sentence it returns says which continuation
the next start will do: all waits, none, or a mix.

`WaitTool` is what writes the mark, and it writes the deadline it was given rather
than its own default: the argument `time` is how long this call waits, and the call is
what has to be continued.
