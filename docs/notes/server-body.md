# server/body.rs

## why the body is written, not built

A request used to be one `Value`: the transcript decoded into items, re-encoded,
measured by character count, and handed to the client. That made the cost of a turn
proportional to the history three times over -- the rows, the items, the string --
and it made a retry send a body that had already been read.

`RequestBody` is a `Stream` of byte chunks instead. The head is the only part
serialized as JSON; every item after it is the bytes the database already holds,
and the array is closed with a tail. Nothing parses the transcript.

## the card is the head's first message

SPEC: "after the system prompt, before the first user message, inject one message with
useful information about session name, harness version, cwd, time." So the request's head
opens with it — one `input` item, ahead of a fold's note and of every row.

Both halves of where it goes are load-bearing, and I got the second one wrong first:

- **Before the transcript, not after.** A value written after the rows is a *new
  message on every step*. A live session read that as "the harness keeps injecting" and
  spent a 45-step turn answering it, and each step re-billed the whole prefix behind it.
- **After the system prompt, not in it.** A provider's cache is taken item by item —
  measured: with the same prose and only the minute changed inside `instructions`, a
  turn answered with 1.2% cached; as its own item, 98.2%. So the volatile part cannot
  live in the `instructions` value.

The card itself is stamped once, with the session's first prompt row, and replayed
verbatim from the store after that: the minute it says is the session's start, not the
current time. A request that has been sent is never rewritten, and a session whose row
predates the card gets one exactly once.

The rule for any future prose: a value that changes every turn goes last among the
*stable* things and never after the transcript; a value that never changes goes first.

## auto-recovering an invalid transcript

SPEC asks for three things at once: everything in a message is stored and sent, a
transcript that has been sent is not changed, and an invalid transcript is recovered
automatically. The provider's own rule is the one that decides what "valid" means, and
it is positional: **a `function_call` is answered by the item after it.** So a step is
one block — its reasoning, then its calls, then one output each, in that order — and
nothing may land inside it.

Read that way, "recover" is not a rewrite of the rows. It is choosing the block:

- **A call nobody answered** — the turn was cancelled, the process died — gets a
  placeholder when its block is written, saying the outcome is unknown. This is the
  auto-fill: the pair is completed at request time, in the request only.
- **A row that is not half of this block** ends the block. An assistant message or a
  reasoning item that arrives while a call is still unanswered belongs to the next step
  and cannot be held in this one, because holding it puts a message between the call and
  its output — which the provider reports as `No tool output found for tool call`. That
  is exactly what a live `config` session got back.
- **A result whose call is not in this block** cannot be placed beside its call, and
  alone it is the item a provider rejects, so it is left out of the request and logged.
  The row stays in the store; leaving a message out of a request is not the same thing
  as changing it.

## Step — the one buffer

A provider reads a `function_call` as answered by the input item directly after it,
while the rows are stored in completion order: a step that asked for two tools
stores whichever result came back first. Replayed in that order, the slower call
has its answer three items away from it, and the request is rejected as an
unanswered call. Because the transcript is replayed whole on every turn, the
session cannot recover from that on its own -- the same `call_id` fails again
minutes later, with only a new `request_id` to tell the attempts apart.

Pairing is therefore by position, not by membership, and it happens here. A step is
over when its last open call is answered -- the model cannot ask for the next one
before it hears about these -- so the buffer holds one step and is then written out
with each result moved up beside its own call. That is what keeps the memory bound
O(step): a reorder that looked across the whole transcript would need the whole
transcript.

## the two ways a pair is broken

A call with no result at the end of the transcript -- the process died inside the
tool -- gets a placeholder saying so. It is the last thing the walk can do, because
only there is "no more rows" known.

A result whose call is not in the same step is left out, logged. A call and its result
belong to one step, so this is decidable inside the step's own buffer without
remembering what was already sent; it names a row whose partner was deleted from
under it, and sent alone it is the request a provider rejects. See above for why
leaving it out is not the same as changing it.

## with_pictures — which field a picture rides in

A row with pictures stops being a string once it reaches the request: the text
becomes a content array with the pictures behind it. The field it replaces
depends on the kind, and only on the kind -- `content` for a message, `output`
for a result -- because those are the fields those two kinds keep their text in.
Everything else goes out byte for byte as stored.

## RequestSource — what a retry replays

The source names a session and a pinned `upto_seq` rather than holding a body,
because a body is consumed by the attempt that sends it. Retries are `LlmClient`'s
own loop, so every attempt opens the same window again: the same rows, in the same
order, for the same request.
