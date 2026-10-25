# bridge/chat.rs

## chat (module doc)

Nothing here knows about websockets or QQ. The state machine is driven by two
inputs -- a message arrived, an event arrived -- and it produces replies. Keeping
that logic pure is what lets it be tested without a platform account.

## DEDUPE_WINDOW

Platforms retry a delivery they think failed (QQ retries if it did not get a
timely answer), so the same id arrives two or three times. Without this, one
message becomes three turns and the transcript gets three identical prompts.

## Dedupe

In memory only: a restart inside the five-minute retry window can re-run one
message. Persisting it would mean a second store next to the harness's, and the
cost of being wrong here is one duplicated turn per restart, not lost work.

## ChatState::last_seq

`None` until the first message: the bridge asks the server where the transcript
ends before it writes to it, otherwise the first turn's read-back would return
the conversation's entire history and the bridge would relay an old answer as if
it were new.

## ChatState::waiting

The same order the server runs its turns in (it queues, it does not interrupt).

## since

The bridge asks the server for a tail window, and the window can reach back into
the turn before: the rows are a fixed range, not "my turn". `since` is what
turns the window into this turn's rows, using the cursor the bridge keeps.

Without it a turn that only ran a tool would relay the *previous* answer as if it
were new -- `final_reply` picks the last assistant text, and that text would
belong to the turn before.

## final_reply

The last assistant text, not a concatenation: a turn that used tools says "let me
look at that" before the tool call and the actual answer after it, and pasting
both reads like the bot is talking to itself. Rows are scoped to one turn by
`since`, not by the query: which rows are still owed is the bridge's question.

## READ_RANGE

The tail window the read-back asks for. A turn longer than this loses its head,
which only `final_reply`'s "it called a tool" fallback notices; the answer itself
is at the tail, so the relay is unaffected.

## final_reply (ordering)

"the last thing it said" is a claim about time, and the caller should not have to
know which direction the window came back in.

## final_reply (a silent turn)

A turn can legitimately end without a word to say -- it ran the tests and
stopped. Silence in the chat would look like the bot crashed, so say what did
happen.

## clip

The harness has already been bitten once by `&text[..max]` on a Chinese string:
slicing on a byte index panics when it lands inside a character, and the panic
took a whole turn down with it (DESIGN §4「模型请求的超时与重试」). Same trap here, so cut on
`char_indices` -- and tell the user where the rest is, because a silently
shortened answer reads like the agent stopped mid-sentence.

## the_same_message_id_is_only_accepted_once

A retried delivery is the normal case, not an error: QQ retries when it does not
get an answer in time, and every retry would otherwise be a second turn on the
same prompt.
