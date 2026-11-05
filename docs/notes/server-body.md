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
useful information about session name, harness version (commit id), cwd, time." So the
request's head opens with it -- one `input` item, ahead of every row.

Four positions have been tried, and each was measured:

- **Inside `instructions`.** A change there costs the index and the whole transcript,
  which is the entire prompt: 24k characters of index edited by one line cost a session
  292k cached tokens (`server-prefix-cache.md`). And a per-session value in the head
  makes every new session's first request one the provider has never seen.
- **Its own item, after the transcript.** A value written after the rows is a *new
  message on every step*: a live session read that as "the harness keeps injecting" and
  spent a 45-step turn answering it.
- **In front of the first user message's text** (its own item folded into it). Tried,
  then reverted: compared side by side with the other two on the body this server really
  sends, the three were indistinguishable -- a new card is cold wherever it sits and the
  same prompt again is ~99.9% (see `server-prefix-cache.md`). So it bought nothing
  measurable and cost a rewrite of a stored row on the way out.

As its own item in front of the messages, the card is the first byte two sessions
differ in, so a provider with a shared prefix cache can serve the tools and the prose and
nothing after the card -- the index included. That is the one cost of this position, and
it is accepted: it is where SPEC puts it, the model reads the facts before the request
rather than after it, and on the provider measured here the position made no difference
at all. Measured on the real server after the change: three of ten fresh sessions of one
directory came back 14,336-14,391 of ~14,440 cached; the other seven were cold, and the
three were the ones not fired within a few seconds of a first send (see
`server-prefix-cache.md`). `carry` in `body` is the one place the text is rewritten, and what it prepends
comes from the stamped prompt row, so a replay sends the same bytes.

The card itself is stamped once, with the session's first prompt row, and replayed
verbatim from the store after that: the minute it says is the session's start, not the
current time. `prompt_in_force` returns that row and never re-renders it -- it used to
re-render on a "move", which a shell reporting its directory under another spelling was
enough to trigger, and each one threw the transcript away.

The rule for any future prose: a value that changes every turn goes last among the
*stable* things and never after the transcript; a value that never changes goes first.

## a valid transcript is made valid on the way in

SPEC asks for three things at once: everything in a message is stored and sent, a
transcript that has been sent is not changed, and an invalid transcript is recovered
automatically. The provider's own rule is the one that decides what "valid" means, and
it is positional: **a `function_call` is answered by the item after it.**

For a year this module resolved that at request time, by buffering a step and
reordering it: results moved up beside their calls, unanswered calls got a placeholder,
rows whose payload had gone were skipped, a picture's File ID was substituted on the way
out. Every one of those is a way for the same transcript to leave as two different byte
sequences, and a provider caches a *byte prefix*, so each one invalidated everything
behind the point it landed. Measured on this machine: one session lost 292k cached
tokens when an indexed file was edited, another lost 315k because a card was
re-rendered for a new spelling of a directory.

Recovery therefore happens where the row is written, not where the request is built:

- **A call nobody answered** -- the turn was cancelled -- is answered by
  `agent::answer_interrupted` at the moment the answer is known to be missing. The row
  says the outcome is unknown, and it is a row like any other.
- **A step is stored in the order the provider takes it**, because a tool call is
  written when it arrives and its result when it returns, and a result is only ever
  appended after its own call. Nothing has to be reordered to make that true.
- **A result whose call is missing** cannot arise from this server any more; if one
  does, it is a row it did not write, and it is sent as stored. Leaving a row out of a
  request is a silent rewrite of every item behind it.

## the sent shape is decided once

`store::wire_item` is the only place a row's request shape exists. It runs inside the
insert, so the `content` column holds the exact bytes every later request carries:
pictures already turned into a content array (with each `@image(...) (id)` replaced by the
File ID the provider minted, while the `display` column keeps what the reader wrote),
and a provider-side item's payload already read back from its blob. A blob that cannot
be read makes the item non-replayable *then*, not at the tenth replay.

`Transcript` is the consequence: a walk that yields the stored items in `seq` order.
`body.rs` writes them between the head and `]}`, comma-separated, and does nothing else.

## RequestSource — what a retry replays

The source names a session and a pinned `upto_seq` rather than holding a body,
because a body is consumed by the attempt that sends it. Retries are `LlmClient`'s
own loop, so every attempt opens the same window again: the same rows, in the same
order, for the same request.
