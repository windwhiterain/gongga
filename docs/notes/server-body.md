# server/body.rs

## why the body is written, not built

A request used to be one `Value`: the transcript decoded into items, re-encoded,
measured by character count, and handed to the client. That made the cost of a turn
proportional to the history three times over -- the rows, the items, the string --
and it made a retry send a body that had already been read.

`RequestBody` is a `Stream` of byte chunks instead. The head is the only part
serialized as JSON; every item after it is the bytes the database already holds,
and the array is closed with a tail. Nothing parses the transcript.

## the card rides in front of the first user message

SPEC: "the first user message carries a card in front of its text: useful information
about session name, harness version (commit id), cwd, time." So a request has no card
item of its own: `RequestBody` puts the card in front of the text of the session's first
user message, the one row `Transcript` names as its carrier.

Three positions have been tried, and each was measured:

- **Inside `instructions`.** A change there costs the index and the whole transcript,
  which is the entire prompt: 24k characters of index edited by one line cost a session
  292k cached tokens (`server-prefix-cache.md`). And a per-session value in the head
  makes every new session's first request one the provider has never seen.
- **Its own item, after the transcript.** A value written after the rows is a *new
  message on every step*: a live session read that as "the harness keeps injecting" and
  spent a 45-step turn answering it.
- **Its own item, before the first user message.** Well-behaved, but it sits inside the
  region a provider serves -- MiniMax serves the prefix in front of the request's *final*
  user message -- and the card is the one thing two sessions cannot share, so no sibling
  could ever warm another's first turn. Measured on one 9.6k-token prompt, the same bytes
  except the card: 128 of 9,586 cached with the card in front, 9,585 of 9,586 with it
  inside the final message.

In front of the first user message's text, the card is part of the turn the user asked
for: the model reads it as that message's preamble, the provider serves the prefix in
front of the final message, and a sibling session's head is then a prefix the provider
holds. `carry` in `body` is the one place the text is rewritten, and what it prepends
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
pictures already turned into a content array (with each `@image:` `(id)` replaced by the
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
