# server/agent.rs

## agent.rs

The agent loop: one user message in, a stream of events out.

A *step* is a single model request plus the tools it called. A *turn* runs
steps until the model stops asking for tools or the user cancels; there is no
step ceiling. After every step the transcript is already durable, so a crash
mid-turn leaves a replayable session rather than a hole.

## DEFAULT_COMPACT_PERCENT

Share of a model's context window that compaction fills before folding the
transcript. The rest is headroom for what a step adds after the check: the
request's own output, and the tool results it asks for.

## Agent::clients

Clients built for sessions that named their own entry, by `provider/model`.

A client builds a connection pool, so a turn that used one per step would pay
for it on every request. Keyed by spec rather than by session because sessions
that choose the same model should share it.

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

Which model this turn talks to, and how much transcript it may carry before
compaction. Resolved once, at the top: a session can be pointed at a different
provider between turns, but every step of one turn has to agree on where it is
sending.

Read once, like the route: a `set_session --thinking-effort` between turns
applies to the next turn, and every step of this one sends the level it started
with.

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

Both come from the session's tuning, falling back to the server's own
configuration. A percentage only has a size once a context window is known,
which is why the window travels with the model.

## Agent::budget

Zero means "off", everywhere and without exception, so a server that wants no
compaction can say so and be obeyed rather than being second guessed by a
default percentage.

Without a context window a percentage has no size. Saying so beats quietly
compacting at a different point than the one that was asked for.

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

How long a boundary takes to arrive is not up to this loop. `run_tools` hands
each call a fresh `inbox_signal`, so whichever call is waiting when the operator
speaks can end its wait instead of sitting out the command -- see
`BashTool::run (a message arriving mid-command)` in `server-tools-mod.md`. One
fresh copy per call is what keeps the same message from waking every call after
it.

## Agent::maybe_notify_context

The loop measures and asks; it never folds. Folding is a judgement about work
only the agent has seen -- which failure was decisive, which output is still
needed -- so the agent acts with `$GONGGA_HOME/skills/heir/transcript`:
`collapse` folds a stretch out of the request, `heir` copies the whole transcript
into a mentor session and empties this one (see `server-compact.md`).

How full the context is, as the provider last measured it. Not an estimate and
not derived from text length: `usage.input_tokens` already covers the
instructions, the tool schemas, the summary and the whole transcript, tokenised
by the party that will bill for it.

The notice names the numbers and the two tools, because a bare "you are near the
limit" leaves the reader to guess what is possible.

Sent once per transcript state, not once per step: a turn that stays above the
threshold takes many steps, and repeating the notice on each would bury the work
it is asking about. `SessionHandle::note_context_asked` remembers the
`summarised_upto` it last spoke about, so folding something -- which changes that
number -- is what earns a second notice.

The notice is queued as an `inject` row and does not touch the inbox signal, so
it never ends a running command's wait the way an operator's message does.

## compaction_budget

`configured` is the server's own number, used only when the roster does not say
how big the model's window is. A known window is the better measure: it makes
the threshold follow the model instead of the other way round.

It is a threshold for *asking*, not for acting: zero turns the notice off.

## Agent::step

Recorded, never computed. `input_tokens` is the provider's own count of what
this request carried -- instructions, tool schemas, summary and transcript --
measured by the tokeniser that will also measure the next request. No arithmetic
over text length lands on that number for every provider we speak to.

A retry is the only news a stalled request ever produces, so it goes to the
operator instead of into the transcript: the turn looks alive, and the reason it
is slow is on screen.

## Attempt

A bundle rather than three more parameters, because the three travel together
and nothing else in the turn needs them: `step` already takes the session, the
turn, the cancellation token.

## a_known_window_beats_the_configured_number

A known window decides the threshold, so a model with a bigger window gets a
bigger budget without anyone retuning the config.

## a_retry_warning_names_why

The line names the cause it now carries, and a ceiling of zero reads as
"until cancelled".
