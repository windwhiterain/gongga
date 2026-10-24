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

The catalog is published to clients at connect time, which is what lets the
CLI's completer stay in sync without a second source of truth.

## DEFAULT_MESSAGE_LIMIT

Enough to hold a whole turn (a turn with 20 tool calls is ~45 rows) without
making a caller read the entire history.

## Dispatch::providers

The machine's LLM roster, which is what `set_session --llm` is checked against
and where its completion list comes from.

## ActiveGuard

The decrement lives in `Drop` rather than at the end of the spawned task, so it
happens even when the turn panics. That matters now that shutdown waits on the
count with no deadline: a task that died on its last line would leave the
counter at one forever and stop the process from ever exiting -- the exact
failure a deadline used to hide.

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

`--llm` is resolved here rather than at the next turn: a typo should fail the
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
`$GONGGA_CLI` call waiting for it -- hanging until that command finished.

## resolve_llm

`provider/model` first, then a bare model id: model ids may themselves contain a
slash, so the fallback is what makes `--llm deepseek-flash` work without making
`--llm goat/deepseek/...` ambiguous.

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

## get_messages --seq

Two readers want two different things from the same rows. A bridge has a cursor
and wants "what arrived after it", which is `--after_seq` + `--limit`: a count
taken back from the tail. A reader that was not there wants "that part of the
transcript", and a *count* cannot say where to stop -- it only says how many of
the newest rows to take. So `--seq` names both ends.

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
- `--seq` together with `--after_seq` or `--limit`, since those name the same
  rows another way and two spellings of one thing is how a client learns the
  wrong one.

The cap is the one `--limit` already had (`MAX_MESSAGE_LIMIT`), not a new budget:
a reply travels over a channel that fragments at 1174 bytes a packet. `--seq`
reuses it and *refuses* rather than truncating, because a silently short window
would make `-590:` look like it worked.

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

`set_session` is where a session's model and compaction point are chosen, so both
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

## get_messages_pages_by_seq

Reading is scoped to a session and paged by sequence number: an IM bridge asks
"what appeared after the seq I left off at", so a wrong default here would make
it resend old answers.
