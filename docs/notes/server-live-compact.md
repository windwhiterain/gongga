# Live compaction, as the server implements it

Clause 3 of the SPEC's Live Compact section is the trigger: the server asks, the
agent writes the summary, and the server commits it with its result. What follows
is how the code keeps that bargain, and the decisions that surprised me.

## The ask backs off

The interval is a threshold, not a switch. With live compact switched on, a step
whose context has grown a whole interval past the last checkpoint queues one
notice; the interval doubles, and the next notice waits for that wider stretch.
An agent that ignores a notice is asked again later instead of at every step, and
one that answers is measured from the checkpoint it just wrote, at the configured
interval again.

The interval in force lives in `sessions.live_compact_interval_effective`, `NULL`
meaning "the configured base", so a restart keeps the backoff. It is raised by
`deliver_pending` and nowhere else: a task the step boundary drops (an incomplete
round, a switch turned off, a context generation that moved) leaves it where it
was, because an ask nobody saw must not raise the bar for the next one.

## A call commits what the transcript says now

A `live_compact` call commits a checkpoint from the transcript as it stands at
the call: the segment starts at the last checkpoint's cutoff (or the origin) and
ends at the newest complete ordinary round, which is kept verbatim. The summary
is the agent's words and carries no row numbers, so nothing has to be frozen when
the ask is queued.

That is what makes the tool usable without an ask: the checkpoint is opened for
every work session, so an agent whose session has live compact switched off can
still fold its own context. A call with nothing to fold -- no origin, or no
complete round since the last checkpoint -- returns a successful result that says
so, appends nothing, and changes nothing.

It used to fail when no ask was outstanding, and the failure was written into the
transcript as an `ERROR` result. That is what invites a retry loop: the model sees
an error, assumes the call was malformed, and calls again -- and the head it
re-sends is paid for. "Nothing to compact" is an answer, not a failure.

## Every work session has a checkpoint

The origin opens in two places, neither of them looking at the switch. A session
that starts in work mode opens it before its first user message (`deliver`, which
is where the first true operator row lands); a session created in explore opens it
at plan finish, the explore -> work transition. A session that never leaves
explore has none, and its `live_compact` calls answer as above.

The opt-in (`live_compact`) therefore governs exactly one thing: whether the
server asks. It cannot govern whether the origin opens, or the tool would be
unusable in the mode the operator chose.

## A summary may cite a file

Clause 4 is a capability: a summary may carry `@file(<path>)`, and the citation
embeds that file whole into the summary, so a later turn reads the content
without opening it again. It is the agent's call, and the notice and the tool's
parameter description say what it costs rather than whether to use it: an
embedded file travels in every later request, and a summary is never a cache hit.

## `next` is a user turn, and `continue` says whether the turn goes on

Both halves are one decision: the checkpoint is context, and who answers it is what
decides whether the model reads it as a continuation or as a restart.

`next` used to be stored as assistant text after the tool result. That made it the
model's own words at the end of the transcript, which is exactly a half step a
request cannot answer, so the next step's `fill_resume_mark` appended the home's
`resume_mark` (`<harness>resume</harness>`) and the model re-oriented as if it had
just been restarted. Measured in one session: 155 of 155 `live_compact` calls
carried a non-empty `next`, and 111 of its 118 resume marks sat directly behind a
row that call wrote. The step that wrote one had `output_chars=0`,
`output_deltas=0`, `tool_items=1` -- the trigger was never "the step had tools",
it was "`next` was stored in the model's voice".

So `next` is `Kind::Inject` with `role: user` now. `answerable_tail` reads the wire
item (a non-assistant role is something the model answers), no mark is filled, and
the request after the checkpoint carries `next` on the user side.

`continue` is the other half, and it mirrors `send_message(continue = false)`:
the default writes the checkpoint and lets the next request read it; `false` ends
the turn there, so the checkpoint is context only and the next message answers
`next`. The call reports the flag on its `Committed`, the step returns
`StepResult::Finished` for it -- but only after every call of that step has been
answered, because a call left unanswered is a provider `400` (see the section on
cuts above; the same rule, the other direction).

Verified on real processes (`.dev/verify/live-compact-continue`): with the flag
omitted the turn sends three requests and the checkpoint's `next` rides the third;
with `continue = false` it sends exactly two, the turn ending `Done` at the
checkpoint, and neither transcript carries a resume mark.

## The schema is stored, not offered per request

See `docs/notes/server-tools.md`: the tool list is a stored row written once, so
the `live_compact` schema is in the head of every session, always -- not gated on the
switch, not gated on the origin, not added while a request is outstanding and
withdrawn after its commit. That entry and exit sat inside the cached prefix and
invalidated the whole conversation at every checkpoint. It cannot follow the switch
either: the head is written once, so a schema that depended on it would be missing
from every session that switched the feature on afterwards, which is exactly the
session that asked for it.

## A cut never splits a call from its output

The origin opens at plan finish, which is the agent's own act inside its turn: the
newest row at that moment is the `tool_call` of the very command switching the mode,
and its output is appended after the origin. Taking `MAX(seq)` therefore recorded a
permanent prefix ending on a call the walk could not complete, because compaction
drops everything between that origin and the latest round.

The request carrying it was refused whole:

```
400 invalid_value  No tool output found for function call:  call_95e31c78e755489594fe7c54
```

Same key, same model, same endpoint: the identical call *with* its output answers
200. So the provider was never at fault, and no other model could carry that
transcript -- every conformant endpoint refuses a request holding half a pair, the
Anthropic adapter included, since it turns the same rows into `tool_use` and
`tool_result`.

Two rules hold it now. `start_live_compact` takes the newest row that is not a
`tool_call`, so a new checkpoint stops at the end of a step. And `Transcript::open`
moves a projection's prefix end back the same way, which is what lets a checkpoint
written under the old rule still be read: the row the checkpoint names is checked
against the cut, not the other way round.
