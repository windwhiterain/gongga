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

`commands::deliver` calls this too, before a turn exists: a message's `@image:`
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

One line for a request that is about to be tried again, naming the attempt, the
ceiling and why the last one gave nothing. `max == 0` reads as "until
cancelled": printing `1/0` said the opposite of what it meant.

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
returns a `ToolOutcome` and emits no chunk, so `index_skill`'s table, a
`check_bash` answer, an argument error and an unknown tool's refusal were all a
block with a verdict and nothing to read -- while the store kept that same output
and a replay of it showed it. Live and replayed then read differently, which is
the drift a shared vocabulary exists to prevent (`docs/notes/cli-lib.md`).

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
needed -- so the agent acts with `$GONGGA_HOME/skills/heir/transcript`:
`collapse` folds a stretch out of the request, `heir` copies the whole transcript
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
limit, so a response that streamed its heir tool cannot restore the old context
or revive its notices. Ordinary injected messages have no such validity condition
and are always delivered.

## notice_budget

`configured` is the server's own number, used only when the roster does not say
how big the model's window is. A known window is the better measure: it makes
the threshold follow the model instead of the other way round.

It is a threshold for *asking*, not for acting: zero turns the notice off.

## prompt_in_force

The prose a session sends is read from the config as it is *now* -- the instructions
verbatim, the notes rendered -- compared with the stored row, and appended only when it
differs. So the row always holds
exactly what the last request carried, and an edit is in effect on the very next
step: a template, a handover note, a directory.

The earlier rule kept the store's row and only re-rendered at a boundary -- the first
step of a turn, a moved note, a changed directory -- so a template edit waited for the
next turn (「重读只重读新入库的，入库的不改」). The user superseded that half on
2026-11-01: 每次读都必须读的是最新的. What survives is the half that was about the
store: rows are append-only and the row sent is the row stored, so a request is never
a rendering nobody recorded.

The cache cost is one missed hit, once, at the edit: prose sits in front of the whole
transcript in the provider's cached prefix, and measured against the fake an edit costs
the boundary step its hit (99% to 35%) with the next step recovering it. Steady state
costs nothing, because an unchanged render is byte-identical to the stored row and
writes nothing. `scripts/replay_audit.py` asserts the other half: for every request
sent, each input item is a stored row or the stored note, and each distinct
`instructions` the provider saw is a `system` row.

## the prompt is prose, an index, and a separate item for this turn

`llm::build_head` composes `instructions` from the prose (`prompts::SYSTEM`, with the
session's own name filled -- the one placeholder, see `server-prompts.md`) and the index
(`index_of`). The start
card (`dynamic_tail`: session, harness build, the first turn's minute, its cwd) is
**not** in that value: it is the request's first `input` item, ahead of a fold's note and
of every row, which is where SPEC puts it and the only place that is both outside the
prose and stable across steps.

Both halves were measured, and the second one cost a live session a turn. Inside
`instructions`: 1.2% cached when the minute changed. As its own item after the
transcript: 98.2% -- but read as a new message on every step, which that session spent 45
steps answering. As the *first* item, stamped once: 99.7%, and nothing to read as an
injection. What the move bought is not cache -- a change inside `instructions` keeps the
bytes before it (`server-prefix-cache.md`) -- but the transcript's own reading: a message
that follows it is a new message, however identical its bytes, and that cost a live
session a turn.

The stronger rule sits above the cache one: **what a request has sent is never
rewritten.** That is why the card is stamped rather than refreshed -- a prompt row is
appended, never edited, and a value that would differ every turn is not re-derived at
all. See `docs/notes/server-body.md`.

## stated

`cwd` reaches the prompt through `stated`, which canonicalises it and strips Windows'
`\\?\` prefix. The reason is measured: the shell adopts whatever spelling the endpoint
reports, so one turn said `C:\...` and the next `C:/...`, which rewrote the tail and cost
the whole prefix -- 2462 tokens of context, 189 cached. One directory now renders one
string, and a re-spelling is no longer a reason to re-bill.

## index_of, rebuilt rather than remembered

Rewritten on 2026-11-02, because rebuilding it from the wrong directory broke SPEC's
invariant. `prompt_in_force` used to pass the process's live `cwd` -- which is
`SessionHandle::cwd()`, the shell's reported directory, refreshed with a `pwd` after
every command. One `cd` inside a tool call therefore rebuilt the index somewhere else
and appended a new system row: the map vanished exactly when the session started
exploring (`cd ~/.gongga` → `AGENTS.md` gone, 21,400 → 12,017 characters), and the
prefix the provider had cached was lost with it.

The index now reads everything in one directory: `home`, the one the prompt row itself
records, which a later move does not change. That covers the seeded `AGENTS.md`, whatever
it names, and the references a *message* names -- a relative path in a message means the
same file for the life of the session, which is what "already sent" has to mean for a map
that is part of what was sent. `scripts/prompt_selftest.sh` holds it: a turn whose tool
call `cd`s into `/tmp` must write no second prompt row, with both a skill and a relative
`@file:` in the message.

The first attempt at this fix kept the live `cwd` for message-named references, on the
theory that a message should be read where it was written. It failed its own probe: a
message naming `@file: docs/notes/tui-main.md` lost that root as soon as the shell `cd`d
away, which is the same rewrite through a narrower door.

One fallback is worth knowing about, because it hid this for a while: when the walk
finds nothing at all, `index_of` returns `None` and `prompt_in_force` replays the
stored row. A moved shell whose message named no references therefore looked harmless
-- the regression only shows when the message names one, which a monitor report always
does.

The first prompt row's second half: the `AGENTS.md` the session's directory carries,
the text of every reference its messages name, and everything those name in turn --
walked recursively, ordered by `prefix_cache::plan`, appended to the instructions as one
string. The rules file is seeded by `meta::roots` rather than named by a message, so a
session is under its directory's rules without having to ask for them. One string rather than an item per entry: the entries are one thing to
plan, a chain in `prefix_cache`, and measured, an item per node re-uses slightly less at
steady state (94.5% against 97.7%), not more.

It runs on **every** request, not once at the first row. An index frozen at the first
turn would keep answering with the bytes a file had then, so an edited `SKILL.md` would
never reach the session that indexed it -- stale text, silently, forever. Rebuilding
costs a read of the files the index already names and nothing else: a build that matches
the stored row writes no row, and `prompt_in_force` compares before appending, so only a
real edit re-bills the prefix. That re-bill is the price of the edit being true.

Two facts are decided here and used everywhere after: the order (the cache's, not the
message's) and `rooted`, the spellings the index carries. `meta::expand` leaves exactly
those literal, and `commands::expand_message` reads them off the stored row -- for a
session with no row yet, it assumes the row about to be written will carry everything
the message names, which is what makes the first turn's references both indexed and
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

## quota at a step's end

SPEC puts `active_quota` in the main column and says it updates at every step's
end, so the same branch that writes `usage` also asks the quota reader to
publish. Which provider that is comes from the client the step ran on
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

## with_time

A tool's result ends with `[elapsed 820ms]`, or `[elapsed 1.2s]` past a second:
the time the call itself took, appended to the output the tool returned. The model
is given no head and no label -- those are how a front-end draws SPEC's
`tool_result{ok, time (spent)}` -- so the only place a model can learn that a call
was slow is the text of the result it reads.

One string for both readers: `output` is what `append_message` stores and what the
next request sends, so a replayed transcript says exactly what the run said.

Measured around `tools.run` alone, so the picture uploads that follow are not
counted as the call's time; `tool_timing` still reports that split, `run_ms`
against `attach_ms`.

`spent` is the spelling, shared with `result_head`: one duration, written one way.

## Attempt

A bundle rather than three more parameters, because the three travel together
and nothing else in the turn needs them: `step` already takes the session, the
turn, the cancellation token.

## failed_warning_names_what_was_dropped

Left silent about discarded text, the line is the one that made this bug
invisible: the operator saw a partial answer on screen and a failure with no
mention of it.

## a_turn_leaves_its_outcome_as_a_row

`events.kind = turn_end` carries the outcome, the step count and the
elapsed time, so "did it finish, and why not" is a query rather than a
match against a log line that carries colour escapes.

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

