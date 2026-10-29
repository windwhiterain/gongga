# Handover: the agent folds or hands over, the server only measures

There is no folding code in this server. `Agent::maybe_notify_context` measures
the context the provider reported, and when it crosses the threshold it queues one
message saying so. Everything else lives in the `heir` skill, installed at
`$GONGGA_HOME/skills/heir/`.

## Why it left the loop

Folding used to live in `Agent::maybe_compress`: at a token threshold the server
called a model of its own with a generated prompt, wrote the note and moved
`handover_upto`. That note was written by a process that had not done the
work — it could not tell a decisive failure from a retry it was about to abandon.

The threshold still exists, because somebody has to notice. The judgement does not.

## Why a script and not a tool

Folding is two statements:

```sql
update sessions set handover = ?, handover_upto = ? where id = ?;
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
tool schemas, the note and the whole transcript, tokenised by the party that
will bill for it.

The notice names the session, the numbers, and the two exact commands. A notice
that says only "you are near the limit" leaves the reader to guess; one that does
not name the session cannot be acted on, because the script takes it as an
argument.

Sent once per transcript state, not once per step: a turn that stays above the
threshold takes many steps, and repeating the notice on each would bury the work
it is asking about. `SessionHandle::note_context_asked` remembers the
`handover_upto` it last spoke about, so folding something -- which changes that
number -- is what earns a second notice.

The notice is queued as an `inject` row and does not touch the inbox signal, so it
never ends a running command's wait the way an operator's message does.

## notice_budget

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
| `--upto` behind the current note | already-folded rows come back into every request, and nothing errors |
| empty note | that stretch leaves every later request while the transcript still looks complete |

The first names the boundary that would work: the nearest earlier message, which is
a `seq` the caller has actually seen.

## A fold resets the measurement

`context_tokens` is what the provider counted for the last request, and that
request carried the prefix just folded. Leaving it alone would make the new context
look full, and the next notice would fire immediately -- on every step. This is the
one line in the script that must not be forgotten.

## heir: the copy is the archive, and emptying is the point

`collapse` changes what a session *sends*. `heir` changes what a session *has*: it
copies every row into another session, verifies the copy, and deletes its own rows --
all but the one this command is running on. The rows are gone from SQLite.

Four things make that acceptable rather than dangerous:

- **The archive is a session, not a file.** A mentor needs no new command, no export
  format and no sync protocol: `get_messages` reads it and `send_message` answers from
  it, which is what every client already does. An archive nobody can query is a file
  with extra steps.
- **The copy is verified before anything is deleted, in the same transaction.** Every
  row is re-read from the mentor and compared on `(kind, content)`; one mismatch rolls
  the whole thing back, including the `sessions` update. So the failure mode "the
  archive is half-written and the original is empty" cannot be reached -- the script
  exits non-zero and the transcript is untouched.
- **The kept row is the rule, not an exception.** The server runs a call the moment it
  streams (see `server-agent.md`), so the session's last row is this command's own
  `tool_call`, and its result can only exist after the process returns. Deleting the
  call would leave that result at the head of the next request, and a provider rejects
  an output whose call is absent -- the whole transcript would fail to send, forever.
  So the cut stops before the pair, `handover_upto` names where it stopped, and the
  instruction the emptied session needs is **not injected as a forged user turn**: it
  is the text this command prints, which arrives as that call's result.
- **The mentor is answered too.** The archive holds the learner's `tool_call`, so the
  script appends the `tool_result` that closes it, and that row is what tells the
  mentor what it has become: read when asked, answer when asked, fold nothing of its
  own -- the notice template repeats the same branch, because a mentor can hit a
  threshold too.
- **Seq restarts when nothing is kept.** `seq` is `max(seq)+1` over the rows that
  exist. When an operator empties a session from a shell there is no call in flight,
  every row goes, and the next one is seq 1 -- so the note must claim to cover nothing
  and `handover_upto` goes to 0. A note left claiming the old last seq would fold
  every row the session ever sends: it would look alive and answer as if it had
  amnesia about everything after the handover.

## "The transcript is never rewritten" meant the server

That sentence was the user's own rule about a live template: reloading
`config.toml` must not reach into a conversation already under way and
edit what was said. It is still true, and the proof is that the store exposes no
writer for a handover at all. What was wrong was reading it as *nothing may ever delete
a row*.

The difference is who acts and when. A config reload rewrites the past without being
asked, mid-conversation, invisibly. `heir` is asked, by the agent that owns the context,
in one step where it also writes the note the next step will run on, and it names the
archive it just made. The server's hands stay off the transcript either way -- which is
why this is a script and not a tool: the tool surface is what the server will do to you,
and the shell is what you chose to do.

## transcript.py — the heir refusals

| Refusal | What it prevents |
|---|---|
| `--session` and `--mentor` name the same session | the copy and the delete cancel each other: the archive is the thing removed |
| an empty `--note` | the session is emptied with nothing left to send, which is memory loss, not a fold |
| a session with no rows | reports instead of writing a mentor whose only content is a marker |
| a copied row that does not match | rolls everything back; the originals survive and stderr names the seq |

A mentor that already has rows is appended to, never overwritten -- a second handover
into the same mentor is a continuation of the archive, and the first handover's marker
row is what tells them apart.

## What the server still owns

Reading the note stays in Rust: `Store::handover` feeds the request, and
`history_item_to_input` puts it in front of the transcript. Writing one has no method
at all, not even behind `#[cfg(test)]`: the tests that need a folded session use
`fold_from_outside`, a second connection issuing the same statement the skill issues.
That is the invariant in code form: **this server never folds.** Deleting rows is not in this file's vocabulary at all;
the only `DELETE FROM messages` that touches a session's own rows is in the skill.