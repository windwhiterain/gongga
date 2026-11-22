# server/agent.rs

## agent.rs

The agent loop: one user message in, a stream of events out.

A *step* is a single model request plus the tools it called. A *turn* runs
steps until the model stops asking for tools or the user cancels; there is no
step ceiling. After every step the transcript is already durable, so a crash
mid-turn leaves a replayable session rather than a hole.

## DEFAULT_CONTEXT_LIMIT_PERCENT

Share of a model's context window at which the server sends the notice. The rest is headroom for what a step adds after the check: the
request's own output, and the tool results it asks for.

## Agent::clients

Clients built for sessions that named their own model, by `provider/model_name`.

A client builds a connection pool, so a turn that used one per step would pay
for it on every request. Keyed by the model rather than by session because
sessions that choose the same model should share it.

## Agent::output_dir

Where capped tool output is spilled. Comes from the config: an agent that cannot
write it anywhere can still cap, it just loses the tail.

## Agent::detached

Detached jobs of every session. Reaches the tools through `ToolContext`, which
is the only place a tool call gets to start or stop one.

## Agent::run_turn

Runs one turn, publishing events as it goes. Never returns an error: failures
are reported on the bus and recorded in the session's status.

`task` is the pending row this turn was started from. Its message is already in
the transcript, so the step-boundary inbox must not hand it back -- that would
append the operator's line twice.

## Agent::resume_turn

Replays a turn that a shutdown interrupted.

The user message is already in the transcript: it is written before the turn
starts, which is the only reason the turn could be recorded as pending at all.
Appending it again made every restart show the operator's line twice, and added
one more copy to the history per resume.

## Agent::turn

A panic anywhere in the loop used to be the end of the turn with nothing left to
clean up after it: the session kept the "running" status it was given on entry,
no request was in flight, nothing was logged, and nothing ever moved again --
which is indistinguishable from a provider that has stopped answering. Catching
it here turns that silence into an error on screen and leaves the session in a
state the operator can do something about.

## Agent::turn_inner

Queue behind whatever is already working on this session. Taking the gate
*before* `new_turn` is the whole point: `new_turn` cancels the running turn, so
a second turn that took it first would kill the turn it is meant to be waiting
behind. Starting a turn used to mean "interrupt"; now it means "get in line".

Otherwise a queued turn looks like the server froze: nothing moves until the
turn in front of it is done, which for a long turn is minutes.

Cancelled while queued. Running now would look like the `interrupt_session` did
nothing, and the message it was going to deliver is dropped just like any other
interrupted turn: whoever spawned this clears the pending row unless a shutdown
asked to keep it.

The wait was behind a `del_session`. There is nothing to write to any more, and
writing would resurrect rows the operator asked to be gone.

Which model the next request talks to, the level it carries, and how much
transcript this session may carry before a notice. All three are read at every
step boundary, not once at the top: a `set_session` is a statement about the next
request, and one step is the longest anything may lag behind it. A turn whose
model changes mid-way therefore switches on the step after the command, and the
pane's title -- which reads the same tuning -- never claims something the next
request will not do.

The cost of reading per step is a roster lookup and a locked read of three
fields; the clients themselves are cached by roster entry, so only a real model
change builds one.

No step budget. A turn runs until the model stops asking for tools, the user
cancels, or a request fails.

There is deliberately no ceiling: a turn that is meant to work for days has no
number of steps that is right in advance. Any number is either too small -- it
stops mid-task and waits for a human to type "continue", which is not autonomy,
it is a babysitter -- or large enough that it is not a limit at all. What
actually keeps a long run finite is the work itself ending, and what keeps it
from spinning is a request that fails, both of which are reported instead of
silently truncating.

Every exit from this loop carries its own outcome, so the value the match below
sees is always set on the way out; there is no default and no fall-through.

Step boundary: everything that arrived while the previous step's tools were
running becomes part of the transcript here, so the request built by `step`
below carries it. Injecting earlier would put a user line between a tool call
and its result, which providers reject outright.

Before building the request: if the last request carried more than the budget,
tell the agent so and stop there. The server does not fold anything itself; the
notice is queued as an inbox row, so it reaches the transcript at this same step
boundary. See `Agent::maybe_notify_context`.

The model says it is done, but something may have landed while it was finishing.
Deliver it and go round again rather than end the turn with a message unread
because it arrived one step too late.

Wrapping rather than saturating: the counter is only a label in logs, and a turn
long enough to reach the end of u32 has bigger things to worry about than the
number restarting.

## Agent::route

Both come from the session's tuning. A session that states no model is refused
here rather than sent on the server's own: the config's `[model]` is what the server
thinks with, never what a session runs on, so the one place that would have made it a
session's model is the one place that says so. A percentage only has a size once a
context window is known, which is why the window travels with the model.

`commands::deliver` calls this too, before a turn exists: a message's `@image(...)`
reference is uploaded by the client its turn will send with, so the id in the row
and the model that reads it are the same pairing. It is `pub(crate)` for that one
caller -- a message must be accepted whether or not a turn is starting.

## Agent::budget

Zero means "off", everywhere and without exception, so a server that wants no
notice can say so and be obeyed rather than being second guessed by a
default percentage.

Without a context window a percentage has no size. Saying so beats quietly
noticing at a different point than the one that was asked for.

## Agent::client_for

Built outside the lock: `LlmClient::new` is the expensive part and two sessions
racing to build the same one is cheaper than serialising every turn behind a
connection pool.

## Agent::fail

Reports a failure that stops the turn before it starts, and leaves the session
in a state the operator can act on rather than "running".

## retry_warning

One line for a request that is about to be tried again, naming the attempt and why the
last one gave nothing. `max == 0` reads as "until cancelled": printing `1/0` said the
opposite of what it meant.

The session is not in the words: the `Event::Log` carries it in its own field and the
client draws the line in that session's window (`client-lib.md`), so naming it again
would be the same name twice. Display only -- it never becomes a transcript row, or the
model would be sent it again on every turn.

## a failed attempt's fragments are dropped before the retry

`llm.stream` retries inside one call, so a retry sends the window pinned when the
request started (`RequestSource::upto_seq`, which is `store.last_seq` at that
moment). Anything a failed attempt wrote after that number is therefore *not* in
the retry's request and cannot be in its answer: it is a fragment of a stream
that died. `step_inner` keeps the number as `window` and, when the stream finally
reports a failure no retry could fix, deletes everything after it with
`store.truncate_after`.

Without this the transcript kept a piece of an answer the model never gave: the
row sat there for every later request to replay, and the operator read it as the
start of the sentence that was never finished. `builtin_call` rows go the same
way -- a provider-side call from an attempt nobody will resume is not a fact
about this turn.

The parts the attempt streamed are closed on the front-end at the same moment
(`done: true` with no text), because a block is a partial answer by definition
and the failure is the end of that answer.

## failed_warning

One line for the operator when nothing more will be tried, in the voice
`retry_warning` uses. It says what the provider said, whether the attempt had
already streamed part of an answer, and how many rows went with it: a reader who
watched six characters appear needs to know they were withdrawn.

## Agent::error

Reports a failure and leaves it somewhere readable. The `Event::Log` is
broadcast and forgotten, so the same sentence also goes into the transcript as
a `notice` -- readable through `get_messages`, never sent back to the model (see
`a_notice_is_readable_but_never_sent_back_to_the_model` in `server-store.md`).
Without it "why did that turn fail" was unanswerable a minute later, which is
how a refusal's reason went missing entirely.

## Agent::deliver_inbox

This is the *only* place a mid-turn message enters a running turn, and it is
called at step boundaries on purpose. A message that arrived while tools were
running cannot be written the moment it arrives: a user line between a
`function_call` and its `function_call_output` is a request the provider
rejects. At a step boundary the previous step is complete, so the user line is a
legal trailing item and the next request carries it.

The row is deleted in the same breath as the transcript write (see
`take_pending_tasks`), so a message is delivered exactly once even if the
process dies in between.

A row with no message cannot be delivered; leaving it would block the queue
forever, and its kind is not one this loop knows how to replay.

## Agent::announce_operator_rows

The operator's own lines go out as `ResponsePart::Input`, the shape
`commands::row_event` replays, and they go out *before* the position that accounts
for them. A window treats `Event::Transcript` as "I have drawn up to here": a line
published after its own announcement is a line every current window has already
declared, and none of them ever fetches it. That is how a message could be stored,
announced, and never drawn.

How long a boundary takes to arrive is not up to this loop. `run_one_call` hands
each call a fresh `inbox_signal`, so whichever call is waiting when the operator
speaks can end its wait instead of sitting out the command -- see
`BashTool::run (a message arriving mid-command)` in `server-tools-mod.md`. One
fresh copy per call is what keeps the same message from waking every call after
it.

## one call at a time, run where it lands

`step` does not collect the calls a response streams and run them after the
stream: each one is executed the moment its `ToolCall` event arrives, so the
transcript never holds a call waiting on a later one, and the call in flight is
always the last row. That ordering is what lets a script that folds or empties
the session keep exactly one row -- its own -- and still leave a request the
provider accepts. A result without its call is a rejected request, so cutting
inside a pair is the one thing no writer may do. The backstop is in the request
builder, not the store: a step's buffer has its own calls, so it can tell an
orphan from a pair and leave the orphan out (`docs/notes/server-body.md`).

## a result's body arrives once

The chunk that ends a call carries the tool's own output only when the tool
streamed nothing: the forwarder counts what it published, and a count of zero is
the one case with no other road to the window. Every builtin except `bash`
returns a `ToolOutcome` and emits no chunk, so `ls_skill`'s table, a
`check_bash` answer, an argument error and an unknown tool's refusal were all a
block with a verdict and nothing to read -- while the store kept that same output
and a replay of it showed it. Live and replayed then read differently, which is
the drift a shared vocabulary exists to prevent (`docs/notes/client-lib.md`).

The count is the fact and not a declaration, because the question is what the
client received: a tool that declares itself streaming and sends nothing is the
same bug as a silent one.

A streaming tool keeps its body as it streamed it. Replacing it with the final
output would be wrong for the paths whose output is not what they printed:
`leave_running` answers "still running after 30.0s ..." while the real output was
the stream, and bash's capped text would retire rows the window already drew.
One body per result, and the verdict's time still rides on the head alone, so the
folded row says exactly what it said before.

## `step_timing` and `tool_timing`

`step` writes one event whether it finishes, stops, or fails. In addition to its
wall time, it records prompt preparation, request-source creation and the full
LLM stream interval. The remainder after those phases is harness time not yet
attributed to one of them. `turn_end` already supplies the enclosing wall time,
so step totals can be compared with turn overhead.

Every executed tool writes one `tool_timing` event keyed by turn, step and call
id. It separates the tool run from attachment processing and records argument
and output character counts, status and absolute start time. These rows can be
joined to `llm_attempt` on `(turn, step)` without parsing log text; overlap is
expected because a tool runs as soon as its call arrives during the stream.

`scripts/timing_report.py` renders the step, attempt and tool ledgers and can
write the full joined data as JSON.

## Agent::maybe_notify_context

The loop measures and asks; it never folds. Folding is a judgement about work
only the agent has seen -- which failure was decisive, which output is still
needed -- so the agent acts with `$GONGGA_HOME/skills/handoff/transcript`:
`collapse` folds a stretch out of the request, `handoff` copies the whole transcript
into a mentor session and empties this one (see `server-handover.md`).

How full the context is, as the provider last measured it. Not an estimate and
not derived from text length: `usage.input_tokens` already covers the
instructions, the tool schemas, the note and the whole transcript, tokenised
by the party that will bill for it.

The notice names the numbers and the two tools, because a bare "you are near the
limit" leaves the reader to guess what is possible.

Sent once per crossing, not once per step: a turn that stays above the threshold
takes many steps, and repeating the notice on each would bury the work it is
asking about. `SessionHandle::note_context_asked` remembers the size it last spoke
at, so two things earn another notice: growing a quarter of the budget past that
size, and a handover, which empties the session and therefore re-arms it.

The notice is queued as an `inject` row and does not touch the inbox signal, so
it never ends a running command's wait the way an operator's message does. Its
payload also records its context generation and token limit, with tokens and
generation read in one statement. A handover's token
reset advances that generation through a database trigger, and usage writes are
conditional on the generation their request started with. `deliver_pending` drops
a marked notice when its generation is old or the current context is below its
limit, so a response that streamed its handoff tool cannot restore the old context
or revive its notices. Ordinary injected messages have no such validity condition
and are always delivered.

## notice_budget

`configured` is the server's own number, used only when the roster does not say
how big the model's window is. A known window is the better measure: it makes
the threshold follow the model instead of the other way round.

It is a threshold for *asking*, not for acting: zero turns the notice off.

## prompt_in_force

The head is stamped once and read from the store after that, without comparison:
if a row exists, that row is what this session sends, for ever. Prose, plan and card
are rendered exactly once, for the one request that will carry them.

The earlier rule re-rendered at every step and appended a new row whenever the text
differed, so that "每次读都必须读的是最新的" held -- an edited template, an edited file the
plan named, and a directory all reached the model on the very next step. That rule is
what this file used to defend, and it was wrong in the way that matters most here: the
head is a *byte prefix*. Any edit to it invalidates every token behind it, and behind it
is the whole transcript. Measured on this machine, before the change:

- 24k characters of plan, edited by one line of `AGENTS.md`, cost session `hierarchy`
  292,992 cached tokens. The next request had a 849-byte common prefix with the one
  before it.
- A card re-rendered because a shell reported `C:\resource\lichen-vm` where the row said
  `C:\c\resource\lichen-vm` cost session `lichen-vm` 315,008.
- The prose was re-rendered at *every* step, so a template edit was never the only thing
  that could move it.

The freshest text is still what the model reads; it now arrives as a row appended to the
transcript instead of as a rewritten prefix (`server-body.md`). A stale plan entry is
the price, and it is one the reader can see and correct, unlike a bill for every token
already sent. `scripts/prompt_selftest.sh` holds the surviving half: a `cd` writes no new
prompt row, and the row that exists names the session, the rules file and every root.

## stated

`cwd` reaches the prompt through `stated`, which canonicalises it and strips Windows'
`\\?\` prefix. The reason is measured: the shell adopts whatever spelling the endpoint
reports, so one turn said `C:\...` and the next `C:/...`, which rewrote the tail and cost
the whole prefix -- 2462 tokens of context, 189 cached. One directory now renders one
string, and a re-spelling is no longer a reason to re-bill.

## plan_of, built once instead of rebuilt

The plan is the first prompt row's second half: the `AGENTS.md` the session's directory
carries, the text of every reference its messages name, and everything those name in
turn -- walked recursively, ordered by `prefix_cache::plan`, appended to the instructions
as one string. The rules file opens it verbatim and is not planned at all: it is read by
`meta::rules` and printed first, so a session is under its directory's rules without
having to ask for them, and no chain the price favours can stand between the model and
what the file says. Only the references it names enter the plan. One string rather than an
item per entry: the entries are one thing to plan, a chain in `prefix_cache`, and
measured, an item per node re-uses slightly less at steady state (94.5% against 97.7%),
not more.

It runs on the request that writes the row, and never again. Whether it runs at all
is the session's `prefix_cache_plan`: a session that states `false` (or follows a
home default of `false`) gets no plan row -- the head is instructions, card and
parameters only, and references travel in the messages that name them. The switch
exists for a small-context model: the plan is stamped once and unmaskable, so a
window it would fill must never be given it. Two arguments had to be
settled for that to be the right answer, and both were settled by measurement:

- **Rebuilding is not free, it is the most expensive thing this server can do.** Each
  rebuild that differed shipped a different prefix, and a different prefix is the entire
  transcript re-billed. `prefix_cache::plan` exists to shave the *tail*; a moving head
  throws away everything that plan saves.
- **Everything is read in one directory**: `home`, the one the prompt row itself records.
  `prompt_in_force` used to pass the process's live `cwd`, so a single `cd` inside a tool
  call rebuilt the plan somewhere else and appended a new row -- the map vanished exactly
  when the session started exploring (`cd ~/.gongga` → `AGENTS.md` gone, 21,400 → 12,017
  characters), and the cached prefix went with it.



Two facts are decided here and used everywhere after: the order (the cache's, not the
message's) and `rooted`, the spellings the plan carries. `meta::expand` leaves exactly
those literal, and `commands::expand_message` reads them off the stored row -- for a
session with no row yet, it assumes the row about to be written will carry everything
the message names, which is what makes the first turn's references both planned and
literal instead of expanded twice. See `docs/notes/server-meta.md`.

## Agent::step

Recorded, never computed. `input_tokens` is the provider's own count of what
this request carried -- instructions, tool schemas, note and transcript --
measured by the tokeniser that will also measure the next request. No arithmetic
over text length lands on that number for every provider we speak to.

A retry is the only news a stalled request ever produces, so it goes to the
operator instead of into the transcript: the turn looks alive, and the reason it
is slow is on screen.

## provider usage goes to the events ledger

Every completed request appends an `events` row of kind `usage` with the model and
`input`, `cached`, `output`, `total`, and logs the same numbers at info -- one line
per model request, beside the `stream ended` line that is already there.

The row, and not only the line, because the question outlives the log: "is the
context this session is paying for actually cached?" gets asked days later by
somebody reading one session back, and `events` is per-session and durable.
`sessions.context_tokens` stays the single number the notice threshold compares
against; the ledger is what explains it.

Nothing reads it through a command. `sqlite3 $GONGGA_HOME/gongga.db "select payload
from events where kind='usage'"` is the whole interface, and a published command
would put a provider's billing detail into the surface the agent is prompted on.

The session row keeps `context_tokens`, `cached_tokens`, `tps` and `ttft_ms`
together, in one statement, because they are what one request measured: SPEC's title
reads all four, and a title assembled from two different requests would be a sentence
about nothing.

The row also carries `ttft_ms` and `tps`, which are this server's clock rather
than the provider's numbers: time from the request to the first delta of any
kind, and output tokens over the time between that first delta and the end of
the stream. SPEC puts the latest pair in a `full` pane's title, and they belong
in the ledger for the same reason as the tokens -- "was it slow, or was it
talking the whole time" is asked after the turn.

Both are read by the task that reads the provider's socket, and closed before the
completion event is queued (`llm::StreamWindow`, taken by `Outbox::complete`).
That is the whole point of them: a step runs a tool call the moment its call
arrives, so the loop that takes the events stops taking them for as long as the
tool runs -- a clock read there counted a ninety-second build as ninety seconds of
generation, and the rate it produced was a property of the tool, not of the model.
The ledger keeps both readings apart: `latency_ms` is still the step's own wall
time around the request, `stream_ms` is the reader's window, and `tps` divides by
the window.

## quota at a step's end

SPEC puts `active_quota` in the main column and says it updates at every step's
end, so the same branch that writes `usage` also asks the quota reader to publish
-- for the provider that step ran on, which is what makes it one of the rows the
block draws. Which provider that is comes from the client the step ran on
(`LlmClient::provider`), not from the session's tuning: a step is what spends an
allowance, and a session that named no `--model` spends the server's.

The call is `quota::publish`, which spawns. Awaiting it here would put a
provider's billing endpoint between the last token and the end of the step,
where nobody is waiting for it.

## outside the usage guard

A provider that reports no counts still took the time, so the wait for the first
token and the rate are measured before the `usage` guard rather than inside it:
the title can say how long a request took to start talking even when nothing
about its billing is known.

## a delta, not a whole list

`Event::Usage` carries the three numbers the `usage` row was written from, per
completed step rather than per turn: SPEC's title says "latest", so it has to be
the last measurement. A delta rather than a whole `Event::Sessions`, because the
client re-announces its layout on every event that changes what is drawn, and a
layout per step would cost every view its scroll position -- SPEC says a view
scrolled away from the bottom stays where it is.

The provider's count is written beside the measured pair, in one statement, and
a request that reports no `input_tokens` keeps the last one it reported: the
number is never recomputed from text, and "unknown" is not an argument for
throwing away a measurement that still describes the session's context.

## a_tool_result_is_what_the_tool_said

A tool's result is exactly the string the tool returned. It used to end with
`[elapsed 820ms]`, appended there because the model is shown no head and no label --
those are how a front-end draws SPEC's `tool_result{ok, time (spent)}`.

The verdict and the duration have carriers of their own now: the head the front-end
draws, and the row's `elapsed_ms`, which `tool_timing` reports against `attach_ms`. A
line in the text was a second copy of a number the answer already carries, and it made
every answer unparseable -- a caller that read a tool's JSON result found `[elapsed
4ms]` after it and could not parse it at all. `spent` stays: `result_head` still spells
the duration, one way, for the label.

## Attempt

A bundle rather than three more parameters, because the three travel together
and nothing else in the turn needs them: `step` already takes the session, the
turn, the cancellation token.

## keep_streamed

What a cancel interrupts is kept. An item is stored by its *completion* event --
`LlmEvent::Output` for assistant text, `LlmEvent::Reasoning` for a thought -- so a
cancel landing mid-item used to leave the chunks existing only on the operator's
screen: sent, then nowhere.

`step` therefore accumulates every chunk by item id, and drops an item from that
map as soon as its completion event stores it. What is left when the cancel arrives
is exactly what no row holds, and `keep_streamed` writes it, marked
`interrupted: true` in the display payload.

Both halves are replayed. Assistant text is a real message and `wire_item` builds an
item from it. A reasoning fragment is stored as the item its deltas describe -- the
text in `content` with `reasoning_text`, a provider's summary in `summary` -- so
`wire_item` sends back an item rather than a guess, and a resumed step reads its own
thought instead of re-deriving it. See `docs/notes/server-store.md`.

## Agent::fill_resume_mark

What a cancel keeps leaves the model's own words at the end of the request, and a
request cannot answer itself: `input` ends on the thought or the half sentence the
stop froze, and the next step is asked to continue from something it was never
given a turn to answer. Resuming such a session therefore restarted the thought from
the same prefix -- a second derivation of the same plan, at full price, for a
320k-token context.

So a request is checked at send time, before its window is pinned: when the last row
a request would send is the model's own (`reasoning`, or assistant-role `text`), the
step appends one `thinking_mark` carrying the home's `resume_mark` template, and the
request ends on a turn after all. Tool output is the other way round -- it is what
the model answers -- so an ordinary step whose tools just ran gets no mark.

The mark is appended, never synthesised while the body is written: SPEC's "auto
validate and fix the latest saved step" is satisfied by a row the next request also
replays, and a body that could invent its own tail is a body that invents a
different one later. One row at most, because what it appends is itself a user turn.
It says nothing about the thought that came before it -- no provider accepts a
half-finished item back -- so it buys a boundary, not the resume of a thought.

## a cancelled call is still answered

`run_one_call` returns early when the cancel token is already set, and that used to
mean a call whose row existed and whose result never would. A provider wants every
`function_call` answered by the item after it, so the next request of that session
was refused outright with "no tool output found for tool call ...", which is how a
cancel turned into a session that could not talk at all.

The early return now writes the `tool_result` row first, carrying the
`interrupted_call` template, and finishes the `tool_calls` row as `cancelled`. That
is the same answer `Step::flush` invents when it builds a request, except this one is
in the store, so the transcript is complete rather than patched at send time.

## failed_warning_names_what_was_dropped

Left silent about discarded text, the line is the one that made this bug
invisible: the operator saw a partial answer on screen and a failure with no
mention of it.

## a_turn_leaves_its_outcome_as_a_row

`events.kind = turn_end` carries the outcome, the step count, the elapsed time
and the turn's answer, so "did it finish, and why not" is a query rather than a
match against a log line that carries colour escapes.

A turn that was cut publishes `turn_interrupted` instead, with the same payload
minus `answer`. A cut turn did not end -- SPEC's `cancel` interrupts it -- so a
subscriber that read `turn_end` as "this turn finished" would take the previous
answer for this one's; the polity watch relayed a cancelled role turn to justice
as if it were an answer. A subscriber that wants both writes a pattern over both
names, and a script that waits on `events --kind turn_end` alone waits forever on
a cut: it must ask for `turn_interrupted` too, or read the session's status. The
monitor's own query is unaffected: it reads `turn_end` for `origin = provider`,
which is a failure, and a failure still ends the turn.

## a_failure_is_a_row_of_its_own

A failed turn writes one row, of kind `error`: the sentence a reader sees and the
turn the model is sent next.

A notice left the reason outside the context, because no request is built from one.
The session's next step resumed as if nothing had happened -- a provider that refused
the answer it was generating, a key at its limit, a stream that never finished -- so
it could neither avoid the cause nor say what went wrong. The parent of a subagent
has the same problem one level up: it drives the child by sending a message, and a
child whose context never learned why its last turn died answers as though the work
had been going on. An injection was bolted onto one of the failure sites instead;
this kind replaces it, because an injection says "the harness is talking" and an
error says "something went wrong", and the reader, the renderer and a replay all
have to tell those apart.

Three things have to agree for an error to be readable, and they did not:

- **it is sent.** `wire_item` gives the row a `user` turn whose text is
  `harness_wrap`ped at write time, so a home that carries no new prompt still states
  the reason.
- **it is drawn.** The writer publishes the `Event::Log { level: Error, session }`
  that draws the line in that session's window, and only then the transcript stamp. A
  client reads a stamp as "I have drawn that row", so a row announced without an event
  is a line no window ever shows (`server-store.md`).
- **it is attributed.** A replay draws it as a line about *that session*, not about
  nobody, which is what an agent reading another session's transcript through
  `get_messages` reads. The line the failure used to leave there said `level: info`
  and named no session at all.

One string, measured once: `Agent::error` formats it and `Agent::write_error` both
stores and publishes it, so the reader and the model cannot drift apart. A panic in a
turn goes through the same door; it used to leave a broadcast and no row.

## a_request_records_what_it_cost_and_how_long

The `usage` row carries the step it belonged to and the wall time the stream
took, retries inside it included: tokens and milliseconds are one experiment,
and neither is derivable from the other. The wait for the first token and the
rate after it travel with it, because they are read from the same clock over the
same stream.

## a_context_notice_is_a_row

The notice is a decision the server made, so it is recorded as
`events.kind = context_notice` with the numbers it quoted. Counting the
injected sentence instead made the count depend on the template's wording.


## Comments moved out of the code

The checker holds a preamble to 120 characters and an `# Invariant` to 400, so the
argument behind a short claim lives here.

**The head is stamped once.** It is part of the prefix the provider caches, so it is
written once and read from the store after that. A template edit, a rewritten
`AGENTS.md` and a re-planned plan all used to re-render it, and each one invalidated the
whole transcript behind it. What the references say is still read every step, but a
change is a row appended to the transcript, never a head that moves.

**There is no exception.** `Prompt::cwd`, `Prompt::remote` and `Prompt::model` record the
facts of the first request, but nothing compares them: a move or a model swap is a
setting, not context. The card may name a machine or model the session has since left,
and the plan may lack a skill the new arrangement would bring in. That is the price of
a byte prefix that never moves, and it is visible to the reader in `messages_between`.
`Store::prompt` reads slot 1, the fixed first row, so what was sent first is what every
later request sends -- no search among rows. The tool schema sits in slot 2 and the first
transcript row in slot 3. A version 5 session, whose head followed its opening line, is
moved into these slots by the 5-to-6 migrator before the store opens it; there is no read
path that walks. A row written before `remote` or `model` existed reads as `None`, which
is a record of a first request that did not carry that fact.

**The plan.** The order is `prefix_cache`'s, which is a permutation of what it was
given: no entry is invented for a reference that could not be read, and the spellings
returned are exactly the ones the plan carries, so `meta::expand` leaves those mentions
literal. Everything is read in `home`, the session's own directory: a `cd` in its shell
must not rewrite what was already sent.

**A failed attempt's window.** The pinned `upto_seq` is also where a failed attempt's
rows end: the retry inside `llm.stream` sends that window, so what the failed attempt
wrote after it is dropped before the next one is sent.

**Three "where"s.** The prose states where a session is three ways -- its name, the
placement a move stated, and the peer whose shells are there now -- because a template
has no conditional. The session's own name is the one value in the prose, so a model
asked what it is called can answer without a tool call.

**Streamed fragments.** The parts already handed to a front-end as blocks are tracked
because a refusal can land mid-item: a block left open there would take the next answer's
text for its own. Nothing follows a failure, since `llm.stream` retries what another
attempt could fix, so what is left said the request itself is wrong and the fragments
this attempt streamed are not its answer.

**Drafts.** Each item's draft is written at most once per `DRAFT_FLUSH`, so a fast
stream is not one disk write per chunk; what a kill may lose is at most that much text.
A part's text is worth keeping before the item finishes when it is an activity --
progress toward an answer not yet given. A call's text is its arguments, which the call
row carries once it is complete.

**Pictures.** A picture's name is read on the machine that printed it: a `bash` call's
picture is here, a `remote_bash` call's is there. The row keeps the text as the command's
words and the copy as the record; nothing is uploaded when the row is written, because
the provider's file is resolved when a request is built, so a replay months later still
has something to name.

**The tier's deadline belongs to the heartbeat.** `cache_fire_after_ms` answers
`window * (100 - margin_pct) / 100` for the session's tier, and `commands::arm_keepalive`
is its only reader: a session that named `keep_alive` gets a time notice, or a dream
in its place, at that point, and the request it starts is what keeps the prefix warm.
No tool is interrupted by it. Until 2026-11-15 one was: `run_before_cache_expiry` cut
a tool's wait so the next request would go out before the prefix went cold, and it
read a window already spent as "now", so a turn starting after one had every tool of
its first step cancelled before it ran -- a `gongga_cli` `send_message` answered
`exit: null` with no output and sent nothing. It is gone on the maintainer's ruling
that `cache_keepalive` already keeps the prefix warm: a tool's wait is the tool's, and
nothing in the turn shortens it.

**One tier per session.** `run_turn` reads `Tuning::cache_tier` once before its
first step, and every step of the turn sends that spelling. A session that named
none falls back to the server's `cache_tier` and then to the shortest tier its
model declares; a name the roster does not declare cannot be stored in the first
place, and the one value every model does take is `none` -- the session stating that
it sends no tier at all, which neither the server's own nor the roster's shortest
answers for. Nothing re-decides the tier between requests. The `usage` row records what
was actually sent, so the observation layer sees the tier that was in force.

## the_next_turn_waits_for_the_ending (2026-11-21)

`SPEC` orders an interrupted turn: the call a cancel ended returns its result, the
loop stops and everything received is saved, and only then is the interrupted flag
set. The ending -- `keep_streamed`'s rows, the `turn_interrupted` row, the status and
the durable turn -- is written by `run_turn` after the loop returns.

So `run_turn` keeps the gate until that ending exists. Released when the loop
returned, the gate was free for the notice about the call the interrupt ended, which
wakes the session at once: the next turn could start inside the window and its row
was saved before the text the cancel preserved. The order was kept in intent and
broken in sequence. The gate is now bound in `run_turn` and dropped last, after
`finish_turn` and the status.

## one start for the row, the event and the timing (2026-11-19)

`started_at` is read once, where the call is announced, and handed to everything that
records it: the published `ToolCallEvent`, the `tool_call` row's payload and the
`tool_calls` row (`Store::start_tool_call` takes it as an argument, so the table no
longer reads its own clock). `run_one_call` takes the same value.

Why: three clocks read a few milliseconds apart are three facts, and the difference
shows up in the one place a person compares numbers -- a window counting from the
event against the `tool_timing` row the observation layer reads. One call, one start.
