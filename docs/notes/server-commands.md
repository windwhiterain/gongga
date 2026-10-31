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
It has three parts -- the server's own commands, the client's (published so no
front-end keeps a copy), and whatever scripts registered -- and one `target`
field on each entry says who runs it. Dispatch reads that field; so does the
CLI. A command nobody implemented is not "unknown" but a catalog entry with no
handler, which the answer says outright.

## Dispatch::resolve

One decision for every kind of command: does the name exist, who runs it, and are
its required arguments here. Client commands are refused rather than executed --
the server cannot change a caller's own state, and calling them "unknown" would
deny an entry that is right there in the greeting it just sent.

## Dispatch::catalog

Rebuilt per greeting, not cached: the script half is a filesystem scan that
re-reads only changed files, so a script added while the server runs appears on
the next connect.

## DEFAULT_MESSAGE_LIMIT

Enough to hold a whole turn (a turn with 20 tool calls is ~45 rows) without
making a caller read the entire history.

## Dispatch::providers

The machine's LLM roster, which is what `set_session --provider` is checked against
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

Three of those fields are not in the row at all: `provider`, `thinking_effort` and
`context_limit` are what the session *would send now*, resolved through the roster
and filled in with the defaults a session that never named them runs on. A title that
showed only what was written down would say nothing about the sessions that name
nothing, which is most of them. `provider` is the id, not the model, because SPEC's
title has a `provider` field and whose account a turn spends is the fact an operator
needs beside the balance row.

## set_session — what the answer says

The confirmation is `session is now <name>` and nothing else. It used to append the
provider spec, the thinking level and the notice threshold, which put
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

`--provider` is resolved here rather than at the next turn: a typo should fail the
command that made it, and the roster can answer with what would have worked
instead of leaving a turn to die later.

A percentage is a percentage of something. If the model has no context window on
record, the number has no meaning -- and quietly keeping the old budget would
look like the setting was applied.

`set_session` doubles as creation: naming a session is the only way to get one,
and refusing to create it would make the command useless.

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

## resolve_provider

`provider/model` first, then a bare model id: model ids may themselves contain a
slash, so the fallback is what makes `--provider deepseek-flash` work without making
`--provider goat/deepseek/...` ambiguous.

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

The range is required, as SPEC spells it: a caller always says which rows it
wants. There is no row limit any more, and no default to guess at.

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

## del_session

Delete means "not this session and not what it is doing", so stop the turn first.
Queueing behind it would make the delete wait for the turn to finish on its own,
which is not what anyone asking for a delete means -- and it is why this does not
simply `claim` and wait.

The turn still has to let go of the transcript before the rows go, so the gate is
taken anyway. After an interrupt it is a wait measured in milliseconds; without
the interrupt it is the length of the turn.

Deleting a session is the one thing that means "the work it started is not wanted
either". `cancel` disagrees on purpose: detached jobs exist to outlive the
server's own shutdown.

## interrupt

Stop one session's turn, and drop what it still owed.

The counterpart to `cancel`, which stops everything but keeps the work: this one
is about a single session and it throws the work away. The difference is a
single pending row, and the distinction is worth a command of its own -- "stop
that, I did not mean it" and "stop, I will continue after a restart" are not the
same request.

It used to be Ctrl+X, which meant only the TUI could do it: a script, and so the
model, had no way to stop a turn it had started. As a command it is reachable by
everyone who can reach the server. See also `interrupt_session`.

Shared by `interrupt_session` and `del_session`. Deleting a session means not
wanting what it is doing either, and queueing politely behind a turn would make
the delete wait for however long that turn takes -- twenty minutes, if that is
how long the turn takes. So delete interrupts first and only then waits, which is
a wait measured in milliseconds.

Not `cancel_and_preserve`. Aborting is deliberate, so this work is not what the
next start owes -- and clearing the rows is what makes that true for the turns
queued behind the running one as well, since they give up once they reach the
front.

## interrupt_session

`peek`, not `get`: a session with no handle in memory is not running anything,
and materialising it would start its shells just to stop nothing. Both "idle" and
"no such session" are still worth saying out loud -- a silent success would read
as "the turn did stop".

## cancel

The SPEC's `cancle`: do not die on the spot. Running turns are cancelled,
whatever they cannot serialise into the database is left as a pending task, and
only then does the process leave.

Detached jobs are the exception and the point of the whole exercise: they were
started precisely so that they would survive this. Killing them here would make
`detached` indistinguishable from `background`, and would take away the one thing
an agent needs in order to replace the server it is running on.

Not `cancel()`: a cancelled turn must still be there for the next start to pick
up, otherwise "cancel, restart, continue" -- the whole point of shutting down
cleanly -- loses the turn it was protecting.

Not quite yet: the acknowledgement announcing what is being cancelled is worth
more than a fast exit, and the listen loop publishes it only after this handler
returns. A quarter second is well inside one round trip.

## nudge_inboxes

The normal path needs no help: a message written while a turn is running is
folded into that turn at its next step boundary. This covers the edge -- the turn
ended in the same instant the message was written, or a background job finished
while the session was idle -- where the row would otherwise sit until the next
`send_message`. A mailbox nobody checks is not delivery.

Starting a second turn for a "turn" row would ask the model to continue for no
reason.

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

## a_panicking_turn_still_releases_its_slot

Shutdown waits for this count with no deadline, so the only thing that can keep
the process alive forever is a turn that never releases its slot. A panic used to
do exactly that, because the decrement was the last statement of the task.

## interrupt_session_names_one_session

Interrupting is per session, and the session list is what completes it. The name
is required: guessing which session to stop is exactly the hidden state the
command surface exists to avoid.

## every_command_takes_a_session_or_a_message

Every command names its session the same way. Four spellings of the same
argument is how a sticky `session` ends up filling only three of them.

## completion_sources_survive_the_wire

The catalog is a wire object read by the client's completer, so every source it
declares has to survive the trip. A value list that lost its variant on the way
out would make Tab silently offer nothing.

## set_session_offers_the_roster_and_a_percentage

`set_session` is where a session's model and notice point are chosen, so both
flags have to be described well enough to complete.

## a_seq_slice_is_parsed_like_a_python_slice

The grammar, case by case, because a slice is the kind of thing that is right
everywhere except the boundary a caller actually uses.

## a_seq_slice_clamps_to_the_transcript

Asking for more than exists is fewer rows. This is what keeps `-99:` usable in a
script that does not know how long the transcript is, and it is also why a range
past the end is a *successful* empty reply rather than an error: "there is
nothing there" is an answer.

## a_bad_seq_slice_is_refused

Every one of these is a typo, and the message names what was wanted. The huge
negative is the arithmetic guard: a bound is `last_seq + n + 1`, so an `i64`
minimum would overflow in debug without saturating.

## get_messages_names_rows_one_way

One spelling, not two. A cursor and a range are rival answers to "which rows",
and the one a caller learns first is the one it will keep using.

## get_messages_pages_by_seq

Reading is scoped to a session and paged by sequence number: an IM bridge asks
"what appeared after the seq I left off at", so a wrong default here would make
it resend old answers.

## a_report_is_one_document

A tool that wants to know what a run cost asks `report` and reads one
document. The alternative is what every adapter did before it: four ad-hoc
queries against tables whose column names are the server's business, which
is how a rename (`summary` to `handover`) broke a benchmark mid-run.
