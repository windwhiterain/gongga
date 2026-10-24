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

Before building the request: if the transcript has grown past the budget, fold
its older half into a summary. Doing it here rather than after a step means the
very next request is the small one.

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

## Agent::maybe_compress

Without this a long turn grows without bound until the provider refuses it
outright.

How full the context is, as the provider last measured it. Not an estimate and
not derived from text length: `usage.input_tokens` already covers the
instructions, the tool schemas, the summary and the whole transcript, tokenised
by the party that will bill for it. The only thing it misses is what this step
adds after the check -- one response plus its tool output -- which is what the
budget's headroom is for.

Asked for by sequence rather than cut out of the whole transcript: the mapping
from rows to items is not one-to-one, so counting rows covered a range that was
not the one the cut named -- and a summary that swallows a message from after
the cut loses it.

The last measured size is now history: it described a request built from the
prefix that was just folded away. Clearing it means "unknown until the next
request reports", which stops the following step from compacting again on a
stale number.

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

## compaction_budget

`configured` is the server's own number, used only when the roster does not say
how big the model's window is. A known window is the better measure: it makes
the threshold follow the model instead of the other way round.

## choose_cut

The constraint is the API's, not ours: a `function_call_output` with no
`function_call` in front of it is rejected, and a reasoning item whose
predecessor vanished is at best wasted. So the boundary is placed where the
next message can stand on its own -- a user turn or a tool call.

## clip

The transcript is full of Chinese, and slicing a `&str` at a byte offset is a
panic when that offset lands inside a character. It used to be, and the panic
took the whole turn down where nothing was left to move it: the session sat at
"running" with no request in flight and no error anywhere, which reads exactly
like a provider that has stopped answering.

## clip_cuts_between_characters_not_bytes

Cutting a summary prompt used to slice by bytes, which panics the moment the cut
lands inside a Chinese character -- taking the turn down mid-flight with no
request in flight and nothing logged.

## cut_never_leaves_an_orphaned_tool_result

The API rejects a `function_call_output` without its `function_call`, so the cut
must never leave one of those at the front of what survives.

Cutting at 4 would leave 5 (a tool_result) first, and at 5 leaves the text on
its own -- that is as far back as it can safely go.

## a_recent_window_of_only_tool_results_is_not_compressed

When nothing in the recent window can start a request on its own, leave the
transcript alone rather than send something the API would reject.

## a_known_window_beats_the_configured_number

A known window decides the threshold, so a model with a bigger window gets a
bigger budget without anyone retuning the config.
