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

`None` until the first message. It is both the probe's answer and the cursor the next
read asks from: the bridge asks the server where the transcript
ends before it writes to it, otherwise the first turn's read-back would return
the conversation's entire history and the bridge would relay an old answer as if
it were new.

## ChatState::waiting

The same order the server runs its turns in (it queues, it does not interrupt).

## The read asks for exactly what is owed

`get_messages` takes a range, and `on_idle` asks from the row after its cursor on:
which rows are still owed is answered by the query rather than by filtering a tail
window after it arrives. A fixed tail reaches back into the turn before, and that is
how a tool-only turn used to relay the *previous* answer as if it were new.

## final_reply

The last assistant text, not a concatenation: a turn that used tools says "let me
look at that" before the tool call and the actual answer after it, and pasting
both reads like the bot is talking to itself.

Rows arrive oldest first and the channel keeps the store's order, so "last" needs
no sorting. What no field says any more is which text row came from the model and
which from the user: a read re-sends a stored row as the part it was streamed as,
and a user's message is stored as an output part too. So `Pending` keeps the words
it handed over and they are excluded here -- a read-back window starts with that
message, and echoing a question back is the one reply a chat user never deserves.

## Row

A read-back row is a streamed part with the session taken off. The part says which
kind of row the store wrote (`text`, `reasoning`, `tool_call`, `builtin_call`), and
that is the only distinction `final_reply` makes: an answer is a `text` row, and a
turn that produced none is a silent one.

A tool's output arrives as a `tool_answer`, which is addressed by its call and not
by a turn -- so it carries no mark that would tell a read's copy from a live one, and
the bridge has no use for either: it relays answers, not stdout.

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
