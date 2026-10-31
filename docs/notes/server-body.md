# server/body.rs

## why the body is written, not built

A request used to be one `Value`: the transcript decoded into items, re-encoded,
measured by character count, and handed to the client. That made the cost of a turn
proportional to the history three times over -- the rows, the items, the string --
and it made a retry send a body that had already been read.

`RequestBody` is a `Stream` of byte chunks instead. The head is the only part
serialized as JSON; every item after it is the bytes the database already holds,
and the array is closed with a tail. Nothing parses the transcript.

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

A result whose call is not in the same step is left out. A call and its result
belong to one step, so this is decidable inside the step's own buffer without
remembering what was already sent; it names a row whose partner was deleted from
under it, and sent alone it is the request a provider rejects.

## with_pictures — which field a picture rides in

A row with pictures stops being a string once it reaches the request: the text
becomes a content array with the pictures behind it. The field it replaces
depends on the kind, and only on the kind -- `content` for a message, `output`
for a result -- because those are the fields those two kinds keep their text in.
Everything else goes out byte for byte as stored.

## tests::a_message_with_a_picture_sends_a_content_array

The text keeps its own part and the picture follows it, by file id: a message and
a tool result reach the provider through the same array, differing only in the
field it replaces.

## RequestSource — what a retry replays

The source names a session and a pinned `upto_seq` rather than holding a body,
because a body is consumed by the attempt that sends it. Retries are `LlmClient`'s
own loop, so every attempt opens the same window again: the same rows, in the same
order, for the same request.
