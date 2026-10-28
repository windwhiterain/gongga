# Compaction: the agent folds, the server only measures

There is no compaction code in this server. `Agent::maybe_notify_context` measures
the context the provider reported, and when it crosses the threshold it queues one
message saying so. Everything else lives in the `compact` skill, installed at
`$GONGGA_HOME/skills/compact/`.

## Why it left the loop

Compaction used to live in `Agent::maybe_compress`: at a token threshold the server
called a model of its own with a generated prompt, wrote the summary and moved
`summarised_upto`. That summary was written by a process that had not done the
work — it could not tell a decisive failure from a retry it was about to abandon.

The threshold still exists, because somebody has to notice. The judgement does not.

## Why a script and not a tool

Folding is two statements:

```sql
update sessions set summary = ?, summarised_upto = ? where id = ?;
update sessions set context_tokens = 0 where id = ?;
```

Reading is one `select` over `kind = 'text'`. A server-side tool for that means a
change to the read shape costs a rebuild, a restart and a cancelled turn — which
is exactly what happened four times while this was a tool. The database is the
interface; WAL makes a second writer safe.

Measured, for the record: 100 external writes against the live server, 0
`SQLITE_BUSY`. The script sets `busy_timeout` anyway, because the one write that
must not fail is the fold.

## Agent::maybe_notify_context

Measures and asks; never folds.

How full the context is, as the provider last measured it. Not an estimate and not
derived from text length: `usage.input_tokens` already covers the instructions, the
tool schemas, the summary and the whole transcript, tokenised by the party that
will bill for it.

The notice names the session, the numbers, and the two exact commands. A notice
that says only "you are near the limit" leaves the reader to guess; one that does
not name the session cannot be acted on, because the script takes it as an
argument.

Sent once per transcript state, not once per step: a turn that stays above the
threshold takes many steps, and repeating the notice on each would bury the work
it is asking about. `SessionHandle::note_context_asked` remembers the
`summarised_upto` it last spoke about, so folding something -- which changes that
number -- is what earns a second notice.

The notice is queued as an `inject` row and does not touch the inbox signal, so it
never ends a running command's wait the way an operator's message does.

## compaction_budget

`configured` is the server's own number, used only when the roster does not say how
big the model's window is. A known window is the better measure: it makes the
threshold follow the model instead of the other way round. Zero turns the notice
off.

## transcript.py — messages, not rows

The unit is the message: what the model said, and what was injected into the
session. `tool_call`, `tool_result` and `reasoning` never appear.

That filter is the whole design. In this repo's own session the rows split 819 tool
calls, 818 results, 314 reasoning and 370 messages: the traffic is 84% of the rows
and close to none of the meaning. Reading it back to decide what to fold was
spending the context the tool exists to protect.

Forty messages with their whole text is about 3k tokens, so the body is returned in
full and there is no preview by default — this is the conversation, and it is what
a handover note has to be written from.

## transcript.py — the three refusals

`collapse` refuses rather than adjusts, because each of these silently corrupts a
request:

| Refusal | What it prevents |
|---|---|
| would leave a `tool_result` without its `tool_call` | the provider rejects the whole request, and it looks like a broken session |
| `--upto` behind the current summary | already-folded rows come back into every request, and nothing errors |
| empty summary | that stretch leaves every later request while the transcript still looks complete |

The first names the boundary that would work: the nearest earlier message, which is
a `seq` the caller has actually seen.

## A fold resets the measurement

`context_tokens` is what the provider counted for the last request, and that
request carried the prefix just folded. Leaving it alone would make the new context
look full, and the next notice would fire immediately -- on every step. This is the
one line in the script that must not be forgotten.

## What the server still owns

Reading a summary stays in Rust: `Store::summary` feeds the request, and
`history_item_to_input` puts it in front of the transcript. Writing one is
test-only (`#[cfg(test)]` on `set_summary`), which is the invariant in code form:
**this server never folds.**
