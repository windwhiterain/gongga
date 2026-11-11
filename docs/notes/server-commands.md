# server/commands.rs

## commands.rs

The command surface: catalog and dispatch.

Stopping work is a command and only a command:
there is no side-channel that cancels a turn, so a script and a keystroke reach
the same code by the same door. Sticky arguments are a client-side concern and
never appear here.

There is no "current session". A command either names its session or fails; the
only way to get a default is the client's `persistant_cmd_argument`, which is
filled in before the command ever reaches the wire.

The catalog is the only list, and it is published to clients at connect time.
It has four parts -- the server's own commands, the tool commands (one per
tool, generated from the registry; see `docs/notes/server-tool-cmd.md`), and
whatever scripts registered -- and one `target` field on each entry says who runs
it. The client's own commands are *not* here: they are the window's, and the window
seeds its own catalog with `protocol::client_commands` (`docs/notes/tui-command.md`). Dispatch
reads that field; so does the CLI. A command nobody implemented is not
"unknown" but a catalog entry with no handler, which the answer says outright.

The catalog also decides the sticky vocabulary: an argument's `kind` is the
entry that fills it (`session`, `model`, `thinking`, `ctx_limit`, `remote`,
`cwd`), and `sticky_to` says which other entry it belongs to -- `--thinking` follows
`--model`, `--cwd` follows `--remote` -- so a reader that keeps the entries applies
SPEC's rule from the published data rather than from a table of names. `set_session`'s settings are all optional with the initials SPEC
spells -- `--model` omitted keeps the session's model (a new one takes the
configured default), `--cwd` omitted stands the session where the server
stands -- so a bare `set_session <name>` creates with every default, and each
setting a caller names becomes a mode the acknowledgement writes back.

## Dispatch::resolve

One decision for every kind of command: does the name exist, who runs it, and are
its required arguments here. A `Client`-target entry that reaches here is refused rather than executed: the server
cannot change a caller's own state. This build publishes none, so the arm is a guard
for a caller from another build rather than a path the window takes.

## Dispatch::catalog

Rebuilt per greeting, not cached: the script half is a filesystem scan that
re-reads only changed files, so a script added while the server runs appears on
the next connect.

## DEFAULT_MESSAGE_LIMIT

Enough to hold a whole turn (a turn with 20 tool calls is ~45 rows) without
making a caller read the entire history.

## Dispatch::providers

The machine's LLM roster, which is what `set_session --model` is checked against
and where its completion list comes from.

## ActiveGuard

The decrement lives in `Drop` rather than at the end of the spawned task, so it
happens even when the turn panics. That matters now that shutdown waits on the
count with no deadline: a task that died on its last line would leave the
counter at one forever and stop the process from ever exiting -- the exact
failure a deadline used to hide.

## session_info

A stored row as a client is told about it. One function, because more than one place
reports the same row -- the greeting and `publish_sessions` -- and two copies of the
fields drift apart the first time one of them asks for another.

Two of those fields are not in the row: `model` and `context_limit` are what the
session *would send now*, resolved through the roster -- `model` into the roster's own
spelling, `context_limit` into the default share. `model` has no further default: a
session that states none reports none, rather than the server's own -- the title would
otherwise name a model the session cannot send on. `model` is the whole model and not
the provider alone: the balance row already says whose account is being spent, and a
title that repeated it without saying which model answered would be the narrower fact
twice.

`cwd` is the live one when the session has a shell, and the row only when it has
none. The row is where the session was *placed*; the shell's own directory is where
every tool reads -- `skill`, `@skill(...)`, `@file(...)` all resolve against it -- so a
row that said the placement while the shell had `cd`-ed elsewhere would let a
front-end complete a name from one directory and send it to another. That is exactly
the bug the client was fixed for: its skills came from the window's launch directory,
because the published `cwd` was the only directory it had and the row's own answer was
the placement rather than the truth.

`thinking_effort` is the exception: it is reported as the row has it, because
A closed argument also publishes `accepts` (`providers::level_accepts`,
`providers::tier_accepts`): the values its command will not refuse, per model where the
roster says something different for each. A tier list carries `none` too, because the
one value every model takes is "send no tier" -- without it a session that inherited a
tier could not state on a model that declares none that it sends nothing.

`set_session` is where a model's roster default becomes a stored level. Filling it in
here as well made the title claim a level the request never carried -- the title said
`thinking high` for every session nobody had given one, while `build_head` sent no
`reasoning` object at all. See `LEVEL_IN_FORCE` below.

## set_session — a stated directory

`--cwd` is resolved to one path before anything is compared or moved:
`session::stated_directory` expands a leading `~` and reads a relative path from a
base. Which base is the whole distinction: a move that keeps the session on this
machine reads it from the session's own directory (the shell that would resolve it is
standing there), a move that brings it to this machine from a peer reads it from the
server's, and a move to a peer leaves it exactly as written -- only the peer can expand
it, and the endpoint canonicalises it anyway.

Resolving before comparing is what keeps `--cwd sub` from tearing down every shell of
a session already standing in `<its dir>/sub`. Resolving before storing is what keeps
the row meaningful later: it is handed to a shell at the next start, and a relative
path there would mean wherever that shell happened to be.

## set_session — must_create
## set_session — mode

`--mode explore` is stored in the session's row and applied before the answer, and the
session is told in one queued message that it changed -- a mode that moved silently
would be a rule the agent never read. The message is a notice (`notice: "explore"`), so
it owes no turn: a person who sets the mode on an idle session gets the setting, not a
turn they did not ask for. See `docs/notes/server-explore.md`.


`--must_create true` refuses the command when the session already exists, and it is
checked before anything else about the session is resolved: a refusal must not have
written a model, moved a directory or ended a shell on its way out.

It exists because the command's normal shape is dangerous in exactly one case. A
`set_session <name>` with no session is a creation; with one, it is a
*selection*, and both answer `session is now <name>`. So a caller that means "start my
own session" and picks a name someone else already used gets no error -- it gets the
other session's transcript, and the next `send_message` puts its brief in the middle of
somebody else's turn. Scripts that fan out work (subagents, archives, per-chat
bridges) are the callers that make this reachable, and they are also the callers that
cannot be trusted to probe first: a probe followed by a create leaves a window in which
the name is taken, and the loser of that race is the one that silently joins.

A boolean spelled `true`/`false` rather than a bare flag, as SPEC writes it: the value
is what a script passes, and a flag that means the opposite of its absence has no way
to say "the default" out loud. See `docs/notes/server-session.md` for what a name may
be at all.

## LEVEL_IN_FORCE

The level a session runs at is resolved once, when `set_session` writes it, and stored:
this command's `--thinking` mapped through the roster, else whatever
`Entry::level_in_force` answers -- the level the session already had when this model
still publishes it, else the level the roster records as this model's own default,
else the highest it publishes. So a session this command creates, and one whose model
it changes, both come out with a level; a command that changes only `--context_limit`
keeps the level the session had.

`--thinking default` is not a level and never reaches the roster: it asks for exactly
what a command that says nothing would leave in force, so a level set earlier is
cleared rather than kept. That is the value the completion offers beside the model's
own, so an operator who set `low` once can go back without recalling which rung this
model tops out at.

Two consequences worth naming. A model that publishes no level (MiniMax-M3 states
`reasoning_default = "off"` and no levels) stores nothing, because `off` there is a
mode rather than a level the request could carry. And resolution at write time is what
keeps the title and the request from disagreeing: `build_head` sends exactly what the
row holds, so a session created before this rule -- one whose row still has no level --
shows no `thinking` field until something sets it.

## set_session — what the answer says

The confirmation is `session is now <name>` and nothing else. It used to append the
model spec, the thinking level and the notice threshold, which put
`(provider deepseek-official/deepseek-flash)` at the top of every new session's window --
the first line of the pane, above the conversation. Those three are SPEC's title
fields now, drawn over the pane, so repeating them in the answer was the same
sentence twice in the place the operator looks first.

The values themselves still travel in the result's `data`, where a script reads them.

## send_message

No implicit current session: the name is either here or the command is wrong. A
default is the client's business (a sticky `session`), not the server's hidden
state.

A message that arrives while this session is working does not start a second
turn: it goes into the inbox and the running turn folds it in at its next step
boundary. Starting a turn would have meant queueing behind the gate -- the
operator's line unread until the agent had finished everything else and stopped,
which is exactly when it is too late to steer.

One read of `busy`, and it decides both things: what contract the row has and
whether this call starts a turn. Two reads were a race with a nasty shape -- the
first said "busy" (so the row was written as "inject", promising the message is
not in the transcript yet), then the turn ended before the second read, which
said "idle" and started a turn that wrote the message *and* left the row behind.
A crash after that made `resume_pending` write the same line a second time, and
"the agent repeated me" is not a symptom anyone can debug from the transcript.

`kind` records which contract the row has. "turn" means the message is already
in the transcript and the row only proves a turn is owed; "inject" means the
message has not been written yet and whoever consumes it must write it. A
restart has to tell them apart.

Remember which row is this message's. Clearing by session used to delete the
rows of messages still waiting their turn: the first turn finished and took the
second one's pending row with it, so a crash before the second turn ran lost the
message entirely -- it was neither in the transcript nor in the pending list.

The turn that was running will fold this in at its next step boundary. If it
ends first, the row is not stranded: `nudge_inboxes` starts a turn for exactly
this case, which is the same path a background job's completion takes.

The turn is done, so it is no longer something the next start owes -- unless a
`cancel` interrupted it, in which case staying pending is exactly what "save
what is left undone" promised.

## send_message (marking the inbox)

`note_message` is what makes "next step boundary" soon rather than eventual.
Waiting for that boundary used to mean waiting for whatever command this turn
happened to be running: a `bash` call asleep for its whole `timeout_ms` held
what the operator said hostage for the entire wait, because one step is the
model's request *plus* every tool call it asked for. Marking the inbox lets
whoever is waiting inside the turn stop waiting -- see `BashTool::run (a message
arriving mid-command)` in `server-tools-mod.md`.

The message itself is delivered by the same code at the same boundary as before;
what changed is how long that boundary is. The row keeps its "inject" contract,
so nothing downstream has to know any of this happened.

## set_session

`--model` is resolved here rather than at the next turn: a typo should fail the
command that made it, and the roster can answer with what would have worked
instead of leaving a turn to die later.

A percentage is a percentage of something. If the model has no context window on
record, the number has no meaning -- and quietly keeping the old budget would
look like the setting was applied.

`set_session` doubles as creation: naming a session is the only way to get one,
and refusing to create it would make the command useless.

Two refusals before anything is created, and both are about the name itself. The first
is `monitor::reserved`: no segment may begin with `@`, because that namespace is the
harness's. The second is `session::checked_name`, SPEC's grammar -- `/`-separated
segments, none empty. Nothing else about the name is refused: a space, a dot and a CJK
segment are all a person's business.

The grammar is enforced here and only here, because this is the one path that makes a
session. Every name after it came from a row this command wrote, so a table that holds
a name is a table whose names have passed -- a monitor's own name never goes through
`set_session`, and `@monitor/error` is a legal value of the grammar's character set
anyway, which is exactly why the reserved check is a separate refusal saying why.

# Invariant

A session's model is the session's. Three sources, in this order: `--model` now,
the model its row already carries, [[DEFAULT_SESSION_MODEL]]. There is no fourth --
the `[model]` section of the config says what *the server itself* thinks with and
never becomes a session's model, which is why the model is written on every
`set_session` that changes anything: a row left holding the config's model is a
session that silently runs on a model nobody chose for it.

The third source is filled, so creating a session without `--model` works and
takes the constant. A build that empties it gets the refusal instead, naming the
flag and `model_pool pick`: a session with no model cannot send a request at all
([[Agent::route]]), and failing at the command beats failing at the first turn.

Checked against the model this session will run on: the entry named here, or the
one it already had. A level the model does not take is a typo in the command
that made it, and the roster can say what it does take while the caller is still
looking at what they typed.

Nothing in the roster describes this model, so there is nothing to check it
against. Passing it through lets the provider answer for itself instead of the
flag being unusable on an unlisted model.

Written down before it is applied: a restart has to bring the session back the
way it was left, and the in-memory copy is only what turns read.

The `cwd` it answers with is read without taking the default shell's lock
([`SessionHandle::cwd`]): this is the one command every operator types at a session,
and a session whose shell is mid-command was enough to leave the answer -- and the
`gongga-cli` call waiting for it -- hanging until that command finished.

## resolve_model

`provider/model_name` first, then a bare model name: model names may themselves
contain a slash, so the fallback is what makes `--model deepseek-flash` work without
making `--model goat/deepseek/...` ambiguous.

## parse_percent

Anything else is a typo, and a typo here would silently change how much of the
conversation survives, so it is refused rather than rounded.

## get_messages

`send_message` is fire-and-forget and streaming events are a delta soup: neither
is a good way for a caller that was not watching to learn "what did it finally
say". An IM bridge has exactly that problem -- it must reply with one message
body, minutes after it handed the turn over. So the transcript itself is
queryable, and the bridge reads the answer it is about to relay rather than
reassembling it from `response` deltas (which interleave with tool calls and
would need the same parsing rules as the UI).

Read-only and cheap: one indexed range scan. It does not claim the session gate,
because reading a transcript while a turn appends to it is exactly what the TUI's
replay does, and neither has anything to write.

## get_messages answers with events

`Outcome` carries an `events` list beside the result, and `run_commands` writes it to
that connection before the `cmd_result`. Only `get_messages` uses it today. It is not
the bus: a broadcast would tell every client on the machine what one client read, and
the read is a private question.

`row_event` picks which event each stored row is, and the rule is what the server
published when that row arrived: `response` for the parts of an answer, `tool_answer`
for a tool's output (addressed by the call that asked for it, so the block is exactly
the one the live stream sealed), `log` for a notice or the prompt in force -- the
harness talking to the reader, which `error` already sends that way. A row with nothing
to show sends nothing.

The result keeps `session`, `status` and `last_seq` and drops the rows, so there is one
way to get a row rather than two. `last_seq` stays because a poller advances its cursor
with it, and the snapshot rule -- rows, status and end from one read -- still holds.

## get_messages --range

There is one way to name rows: a range. The old `--after_seq` + `--limit` pair
was this range plus a truncation policy -- "the newest n after the cursor" --
and a truncation policy is not something a terminal should hold: it silently
answers a different question than the one asked, and the caller cannot tell.
Both are gone.

A caller with a cursor (the IM bridge) asks from the row after it on: `12:` is
"what I have not been answered for yet", which is the range's own words rather
than a tail window filtered after it arrived. A caller reading a part of the
transcript asks for exactly that part.

The range leads the command line, as SPEC spells it, and may be omitted: an
omitted range is the whole transcript. The session is the `--session` argument,
required, filled from the sticky entry like any required argument.

The syntax is Python's, with two differences that are deliberate:

- **Both ends are inclusive.** `5:10` is seq 5 through 10. Python's half-open
  range counts the gaps between elements; here every number names a row, and the
  unit is the row, so excluding the one the caller typed would be the surprise.
  The reply always states the range it actually returned (`3 row(s) at seq 7..9`),
  so a reader never has to remember which convention this is.
- **A negative bound counts back from the newest row**, resolved against the
  session's `last_seq` at read time: `-1` is the newest row, `-5:` the newest
  five, `-5:-2` the five-to-second-newest. This is the part Python and this
  command agree on exactly.

Everything else behaves like a slice: an omitted bound takes its end of the
transcript, and asking for more than exists is fewer rows rather than an error
(`-99:` on a ten-row transcript is all ten). A range that starts past the tail is
an empty window, and the reply says where the transcript actually ends.

What is refused, because each is a typo rather than an intention:

- a second colon (`1:2:3` has a step, and nothing here steps);
- `0`, in either direction of spelling -- seq starts at 1, so 0 names no row;
- a range written backwards (`10:5`, `-2:-5`);
- `:` with neither bound, which names everything and is not what anyone meant;

The one limit left is the channel's: 8 MiB per message, because that is what it
can carry. A range bigger than that is not refused by this command -- it is
answered, and the channel says it cannot carry the answer. That refusal arrives
as a `cmd_result` naming both sizes, not as a dropped connection, which is what
it used to do.

## clone_session

A session written into the database behind the server's back is a session no window
ever lists: the tree is drawn from the `sessions` event, and a script's `insert` sends
none. The heir copied its transcript that way, which is what this command exists to
replace -- the copy is the server's act, so the server can say it happened.

Two refusals, both structural. The source must exist, and the target name must be free
(`UNIQUE` on `sessions.name`, reported as "already exists"): a copy that joined a
transcript already under that name would be two conversations in one window, the same
reason `set_session --must_create` exists for a caller that means to start its own.

What is published is what a client needs to see the copy as it sees the source: the
session list first, then every copied row as the event it streamed as -- `row_event`
with `REPLAY_TURN`, the same rule a read answers with. This is the bus and not
`Outcome::events`, and that difference from `get_messages` is the point: a read is one
client's private question, while a clone is a session every client's tree should hold.
A failed re-send is a warning in the log, never an undo: the rows are already durable,
and a clone nobody was told about is still a clone.

## del_session

Delete means "not this session and not what it is doing", so stop the turn first.
Queueing behind it would make the delete wait for the turn to finish on its own,
which is not what anyone asking for a delete means -- and it is why this does not
simply `claim` and wait.

The turn still has to let go of the transcript before the rows go, so the gate is
taken anyway. After an interrupt it is a wait measured in milliseconds; without
the interrupt it is the length of the turn.

Deleting a session is the one thing that means "the work it started is not wanted
either". `cancel` disagrees on purpose: background jobs exist to outlive a cancel,
and a delete is not one.

Nothing in a delete waits for a process. What a session owned is taken out of its
hands -- its tool jobs cancelled, its detached bash taken off the registry, its
shells handed to a task that closes them -- and the rows go in one transaction
with the session. What stays is a log line per process that would not stop: an
answer that waited for `kill_tree` to confirm a tree could be delayed by any job
that refused to die, and a delete that refuses is a session the operator cannot
get rid of.

## interrupt

Stop one session's turn. The token is all this does: SPEC gives the canceled flag
to `cancel` alone, and `cancel` writes it itself, for every running session, before
the first token fires.

Three verbs stop a turn and SPEC gives each its own aftermath, so the caller says
which one it is (`Stop`): `Cancel` keeps this turn's rows and lets a restart
continue it; `Interrupt` ends the turn where it stands and keeps nothing for a
restart; `Delete` owes nothing at all. The verb is recorded on the session before
the token fires, because the status a window is told is read from it. No tool call
reads it: SPEC's answer to a waiting call is "make it early return", so what a call
answers is what it has, never the name of the verb that ended the turn.
That word is the whole difference between "idle (cancelled)" and "idle
(interrupted)": before it existed, an `interrupt_session` was reported as a cancel,
with a sentence about a server that was not stopping and a turn a restart would
have continued.

It used to be Ctrl+X, which meant only the TUI could do it: a script, and so the
model, had no way to stop a turn it had started. As a command it is reachable by
everyone who can reach the server. See also `interrupt_session`.

Shared by `interrupt_session` and `del_session`. Deleting a session means not
wanting what it is doing either, and queueing politely behind a turn would make
the delete wait for however long that turn takes -- twenty minutes, if that is
how long the turn takes. So delete interrupts first and only then waits, which is
a wait measured in milliseconds.

The token, not a boundary flag: a call stuck in a shell write has no boundary to
reach. What a stop clears is only what the session no longer owes -- an idle
session's leftover `turn` row -- and never the inbox: a queued message is received
input, and everything received must be saved to context. A busy turn's own row
stays for `finish_turn`, which keeps it for a cancel and drops it for an interrupt.

## interrupt_session

`peek`, not `get`: a session with no handle in memory is not running anything,
and materialising it would start its shells just to stop nothing. Both "idle" and
"no such session" are still worth saying out loud -- a silent success would read
as "the turn did stop".

SPEC asks this verb for no flag: for a waiting tool call it makes an early return,
and it stops the loop with everything received already in context, so the turn is over
rather than owed. A marker here would have a later restart finish a turn that had
already ended. `cancel` is the only writer of the flag, and `sessions.canceled` is where
it lives rather than the process: a stop ends the process, and a marker that cannot
outlive it is a marker the next start never reads.

"Over rather than owed" is two things in the code, and this verb has to do both or it is
a cancel wearing another name: no `mark_resumable` (so `finish_turn` drops the turn's
pending row instead of leaving it) and `mark_interrupted` (so a window is told
`interrupted` rather than `cancelled`). The call itself needs nothing from here: its
token ends, it returns what it has, and the sentence is its own -- see
`docs/notes/server-tools-mod.md`.

The inbox is not the turn's to take: a message queued while the turn ran survives
the interrupt, and `nudge_inboxes` starts a turn for it -- the same path a turn that
ends first takes. A queued message is received input, and everything received must
be saved to context, so no stop but a delete may drop it.

## cancel

SPEC's `cancel`: interrupt every turn and leave. `cancel` writes one row per
running session before the first token fires, and SPEC splits them: one that was
running and not waiting is canceled (`sessions.canceled`), so the next start
continues it; one inside `wait` gets the clock that wait was due to end at
(`sessions.waiting_until`) and keeps its rows, so nothing queued behind the wait
is lost and the wait itself is finished later rather than skipped.

That clock is a ceiling, not a schedule. A `wait` ends on what arrives, and a `cancel`
ends every channel -- each one's end notice is a `channel` inject row -- so a session
whose inbox already holds rows has what it was waiting for: the next start clears the
flag and starts it at once. Only a session with nothing to read sits out the time that
was left. Without that, a stop that ended the very channel a session was waiting on
would leave it asleep for the whole remainder, with the answer already in the queue. An idle session
gets neither -- it has nothing to continue. The listen loop breaks as soon as
`shutdown` is pulled, without waiting for `active` to drain -- a call stuck in a
shell write or a model stream has no step boundary to reach, and a flag it may
never read is not worth holding the port for.

Background jobs are the exception, and only for this one ask: they were started
precisely so that a `cancel` would let them go. Killing them here would make
`background` indistinguishable from foreground work, and would take away the one
thing an agent needs in order to replace the server it is running on.

Only a `cancel` that crossed the wire does this. A window closing, a console
interrupt and the end of the process this server follows reach `commands::stop`,
which calls the same `cancel` through `Origin::Signal` -- and SPEC's "kill server
kills all jobs" owns that path. `DetachedRegistry::stop_all` ends every armed tree,
workers included, and stops the registry's pids as well for a tree that failed to
arm. The rows are left for the next start, which reports a job killed while the
server was down. `release_detached` is the one act that lets a job object go, and a
client `cancel` is its only caller. The `server_stop` row says who asked and records
`released` or `killed`, so the difference is a fact in the log.

The acknowledgement announcing what was interrupted is worth more than a fast
exit, and the listen loop publishes it only after this handler returns. A quarter
second is well inside one round trip.

### Who asked

`cancel` records the ask once the turns are interrupted: an `events` row of kind
`server_stop` whose `payload.by` names the asker and whose counts say what is being
stopped. Publishing that row is the notice -- it is how a window that did *not* ask
learns the server is stopping, and it is the only place that says who asked. The
asker's own result answers the asker's connection; the status line that follows from
the row states the fact, not the name.

`Origin`, in this module, is the one place that answers "who". A command that
crossed the wire is a client: the label it gave in its hello, the peer it dialled
from, and the session the command was run from when a shell ran it -- which is also
the only thing `subscribe` ever needed to know. A stop that never crossed the wire
carries what reached the process instead: which console event, which signal, or the
end of the process this server follows. See `docs/notes/server-signal.md`.

## subscribe

Arms a pattern for the session it was run from. The command answers at once: it
registers rather than waits, and the news goes to the session as an inbox row --
see `docs/notes/server-subscriptions.md` for why waiting is the `wait` tool's job
and not a command's.

The session is the *caller's*, never the pattern's. It comes from
`ClientInfo::session`, which the client reads off `$GONGGA_SESSION`: who a command
acts on (`--session`) and who ran it are two different questions, and this is the
one command that needs both answered separately. A client that names no session is
refused, because a subscription with no addressee would fire and tell nobody.

`--persistant` is whether it survives its own match: without it one match is the
whole subscription, and the row is deleted as it fires. A pattern meant to fire
every time says so, and one that only ever needed to happen once stops costing
anything after it does.

## nudge_inboxes

The normal path needs no help: a message written while a turn is running is
folded into that turn at its next step boundary. This covers the edge -- the turn
ended in the same instant the message was written, or a background job finished
while the session was idle -- where the row would otherwise sit until the next
`send_message`. A mailbox nobody checks is not delivery.

Starting a second turn for a "turn" row would ask the model to continue for no
reason.

A *notice* -- one of the server's own rows, `{"notice": "background"}` for a job
that ended or `{"notice": "finished"}` for a command that outlived its wait -- is
news this session is owed, not a person waiting, and it owes a turn on its own
too: a session that is idle has nobody else to read it, and SPEC's `wait` is
answered by exactly this kind of row. A wait a `cancel` kept needs no notice of its
own: its `turn` row is what wakes the session on the next start, which is where the
news it was waiting for arrives. The
one exception is `{"notice": "explore"}`: a reminder about the turn already
running, which an idle session would be woken to read about a turn it is not in.
See `docs/notes/server-explore.md`.

`None` as the owning row: this turn was not started from a message, so the
step-boundary drain may take every queued row it finds.

## resume_pending

A turn is replayed by asking the model to continue: its transcript is already
durable, so this resumes rather than restarts.

Kept for the log only: the turn is replayed from the transcript, not from this
payload, so the message must not be appended twice.

An "inject" row was never delivered: it was written while a turn was running, and
that turn (or the process) ended before the step boundary that would have folded
it in. Write it now -- the resume below replays a turn that reads the transcript,
and a message that is not in the transcript would be replayed into nothing.

Only this task is finished. Clearing the whole session would drop tasks a
second, still-pending turn of the same session recorded.

A `"background"` notice is dropped here rather than replayed. It says "what it
printed is in that shell's unread buffer", and the shell died with the process
that wrote the row: the buffer cannot be read by anyone, so delivering it wakes a
session to be told to look somewhere empty. How many went is logged per session.
The `"finished"` and `"detached"` kinds carry their output -- or the path to it --
in the row itself, so they survive and wake the session that was waiting.

## get_messages_pages_by_seq

Reading is scoped to a session and paged by sequence number: an IM bridge asks
"what appeared after the seq I left off at", so a wrong default here would make
it resend old answers.

## a_report_is_one_document

A tool that wants to know what a run cost asks `report` and reads one
document. The alternative is what every adapter did before it: four ad-hoc
queries against tables whose column names are the server's business, which
is how a rename (`summary` to `handover`) broke a benchmark mid-run.

## `@` is the harness's namespace

`set_session` refuses a name beginning with `@`. Built-in sessions live there --
the harness's monitors are `@monitor/error` and `@monitor/disk` -- so the namespace
is reserved, and the harness never has to look at a session and decide whether it is
its own. Refused rather than
renamed: a session under a name the caller did not choose is a session they will
not find again.

Deleting one is still allowed: a monitor that cannot work has to be removable, and
the next failure creates it again.

`#` was the first choice and lost on a detail: a word that *starts* with `#` opens a
comment in `shell_words::split`, which is what splits the input line and a script's
argv, so `--session '#monitor'` had to be quoted or written `--session=#monitor`.
`@` needs neither. It costs nothing on the reference side either: `@image(...)`,
`@skill(...)` and `@file(...)` match on an opening parenthesis, so a bare
`@monitor/error` is never read as one (`meta::mentions`).

## a session's own heartbeat

`spawn_turn` reads `Tuning::cache_keepalive` before `run_turn` starts. A session
that never asked for it starts no timer. One that did gets, at the end of every
turn, a timer of `window * (100 - margin_pct) / 100`, where `window` is its own
tier's declared lifetime. When the timer fires it delivers
`(time notice: 2026-11-04 09:57 UTC)` -- `clock::now()`, the same spelling the
prompt carries -- through the ordinary `deliver` / `begin_turn` path. The notice
is a real message at the tail of the transcript, where a new item does not
disturb the prefix.

Every turn ends by scheduling the next notice, including a turn that was itself
a notice, so the chain has no last one. Two guards keep the timers from piling
up: the timer checks `last_usage_at` against the turn's end, so a request during
the wait has already refreshed the prefix and the older timer retires; and it
checks the session's busy flag, so a running turn owns the next timer. A session
set to `cache_keepalive false` stops the chain at its next turn end.

The timer is a task, not a process. A server that shuts down takes it with it;
nothing survives the turn to be discovered later.

## How an argument is spelled, and what a `path` means

SPEC names three roles and gives each one word, so the catalog does too: `session`
is the session a command acts *within* (`send_message`, `get_messages`,
`ls_channel`, `ls_skill`, `run_skill`), `id` is the one entity being addressed
(`set_session`, `interrupt_session`, `clone_session`, `del_channel`,
`rename_channel`, `new_channel`, `input_channel`, `run_skill`), and `path` is a place
in a namespace, which is a subtree rather than a name (`ls_session`, `del_session`,
`ls_channel`, `ls_subscription`, `del_subscription`, `ls_skill`). `subscribe`'s
optional name is `id`: SPEC writes `id: option[Subscription] = none`, and it stays
named-only because an optional first positional cannot be told apart from the first
required one.

A path is a component match, not a string prefix: `a/b` is under `a`, `ab` is not,
and empty is every path. That one rule (`commands::under`) is what makes deleting a
path delete what is under it -- `del_session main` takes `main` and `main/a`, a
`del_channel c/x` ends `c/x/1` and `c/x/2`, `del_subscription hook` drops `hook/one`
and `hook/two` and leaves `other`. The deletes themselves stay per entity: each
session's delete ends its own shells, jobs and channels, so the subtree is walked
one at a time rather than batched.

## ls_skill

SPEC's `## Python Cmd` gives skills a namespace, so a skill has a path and the path
is `<bucket>/<name>`: `project/evolve` for the checkout's own, `harness/heir` for the
home's, `general/polity` for the machine's. `skill::Skill::id` is that spelling and
nothing else, which is what lets a row this lists be pasted into `run_skill`.

The walk is the one `skill` and `@skill(...)` already make (`tools::skill::installed_for`):
the project layer needs the session *on this machine*, so a session whose directory is
on a peer is handed the home's and the machine's rows instead. `path` is filtered with
`commands::under`, so `harness` is every skill the home installed, `harness/heir` is
one, and `har` is nothing -- a component, never a string prefix. The rows are the tool
table's own (`tools::skill::row`), so the two faces never disagree about an `id`.

## run_skill

`run_skill(id, cmd, session)` is SPEC's call: `cmd` is `{name, args}`, the same shape
`subscribe` runs on a match, and `parse_cmd` reads it the same way. The id resolves
against the same walk and picks one root by name, which is the reason the id exists:
`skill` refuses a name two roots spell, and a path never does.

It then starts nothing itself. The words `[name, --key value, ...]` go to the `skill`
tool through `tool_cmd::dispatch_tool`, so how a package is imported, where its
`gongga_skill` comes from, what the entry's exit code means and how much output it may
print are decided in one place. A skill that is a body or a script entry has no
functions to choose between, so it is refused here and named for `skill` instead of
being guessed at; a function the package does not publish is refused by
`gongga_skill` with the signature and the list of what it does publish.

`run_skill` fills the tool's `id` argument, which is why the tool accepts one at all:
the catalog never offers it to a model, so the tool's own sentence still begins at
`name`.
