# server/llm.rs

## llm

Streaming client for the OpenAI Responses API.

The wire format is server-sent events over a POST to `{base_url}/responses`.
We parse it by hand rather than pulling in an SSE crate: the event set we
care about is small, and owning the parser means we can surface partial
tool-call arguments to the UI as they arrive.

## LlmClient::new

The timeout lives on the client so every request is covered, the compression
call included. It is what turns "the provider went quiet" from a turn that
hangs forever into a retryable error.

## LlmClient::model

The model this client asks for.

Logged at the start of every turn, so "which model answered this one" is a
fact rather than something to infer from the transcript.

## LlmClient::stream

`summary`, when present, stands in for the part of the transcript that has
been compressed away; it is sent as the first item so the model reads it
before anything else.

A request that fails, times out, or completes with nothing in it is retried
with a doubling delay, because all three look identical from here and all
three are the provider's bad minute rather than ours. The caller sees one
stream either way; a retry that is about to happen is announced as
`LlmEvent::Retrying` so the operator can watch it happen.

An empty completion is worth asking for again: the provider acknowledged the
request and then said nothing, which is what an overloaded or truncated
response looks like. So is a stream that stopped without completing.

## LlmClient::build_body

The Responses API's spelling for "how hard to think", which the roster records
per model as `reasoning_field.responses`. It is the only surface this server
speaks, so there is nothing to switch on.

## LlmClient::summarise

Asks for a plain-text answer: no tools, no streaming. Used only to compress old
context, which must never itself trigger a tool call.

Retried on the same terms as a stream: an empty summary is not a summary, and a
provider that swallowed the request once often answers the second time.
`max_retries == 0` is no ceiling, so the turn's `cancel` is what stops it --
compression must never outlive the turn that asked for it.

## SUMMARY_MAX_TOKENS

Budget for a compression request. The summary stands in for tens of kilobytes,
so it only has to be a page or so itself.

## extract_text

Pulls the assistant text out of a non-streaming Responses payload, falling back
to a slice of the body so a surprising shape is still diagnosable.

## LlmEvent

What the model produced, one chunk at a time.

`item_id` is carried on every variant because a single response can interleave
several output items; the loop does not need it today but the identity is the
thing that makes the events unambiguous if it ever does.

## LlmEvent::Retrying

The previous attempt produced nothing usable and another one is coming after
`delay`. Announced rather than hidden so a stalled turn reads as "retrying"
instead of "thinking".

## retryable

Whether another attempt is worth making.

`produced` is the only thing that rules one out: once the model has said
anything, asking again would stand a second, different answer behind the first.
Everything else -- an error, a timeout, a completion with nothing in it -- is
the provider's bad minute and deserves another try.

`max_retries == 0` is *no ceiling*: a long-running turn is not allowed to die on
a transient outage at hour three, so the default retries until the turn's cancel
token fires. The cancel is the only honest stop -- nothing about a bad
provider's recovery time is knowable in advance.

## stream_once

One attempt at a streaming request, handed back as a channel.

Split out from `LlmClient::stream` so that a retry is just another call: no
attempt carries state into the next one, and the caller drains each the same
way.

Silence counts as failure. A connection that is still open but has stopped
sending is exactly the state that used to park a turn forever: no error, no EOF,
nothing to wake up on.

A stream that ends without its trailing blank line still sent a final event;
dropping what is left is how the last `response.completed` goes missing and the
turn looks like the model said nothing.

## ItemMeta::text

Text accumulated from deltas. Providers do not all return the finished text on
`output_item.done` (MiniMax sends an empty `summary` array), so the deltas are
the only place the readable text ever exists.

## next_sse_block

Takes one complete server-sent-event block off the front of `buf`.

A block ends at a blank line, and providers do not agree on what a blank line
is: `\n\n`, `\r\n\r\n`, and even `\n\r\n` all occur. Knowing only one of them
looks exactly like silence -- no block ever completes, so nothing is parsed and
the turn dies on the idle timeout with "no response data", which blames the
provider for a separator nobody promised to send.

The block it returns has its trailing `\r` stripped, so the caller only ever
sees `data:` lines.

## handle (reasoning deltas)

Not every provider emits `output_item.added` before the deltas, so create the
entry if it is missing: `output_item.done` sorts out the kind from `type`
regardless of how we got here.

## handle (output_item.done)

The readable summary is the provider's if it sends one; otherwise the deltas we
accumulated are all there is. MiniMax is in the second camp: its `summary` array
on `output_item.done` is empty, so without this every stored reasoning entry
would have a blank summary forever.

## history_item_to_input

The stored `summary` is for humans: the UI replays it when a client reconnects.
It is deliberately *not* sent back to the model, which already has its reasoning
state in the encrypted blob -- echoing the summary too would put every thought
into `input[]` a second time.

Replaying reasoning requires the encrypted blob; without it the item is dropped
rather than sent in a form the API would reject.

## tests::sse_blocks_end_at_any_blank_line

Every separator a provider might use ends a block, and the `\r` does not travel
into the parser; a block split across two reads is only complete once its
separator lands.

## tests::every_request_carries_the_tool_schema

The tool schema is not part of the transcript, so nothing a compaction does can
remove it -- but that also means nothing would notice if the body stopped
carrying it. It is rebuilt from the registry every request, and this pins that.

A compacted prefix must not change what the model is offered: the schema is out
of band, not part of the history.

An empty registry means no `tools` key at all, rather than an empty array that
some providers reject.

## tests::reasoning_summary_falls_back_to_accumulated_deltas

MiniMax streams the readable thinking as deltas and hands back an empty
`summary` on `output_item.done`. Storing that would mean every reasoning entry
in the transcript has a blank summary, so the deltas are the fallback.

## tests::a_supplied_reasoning_summary_is_kept

When the provider does send a summary, it wins: it is the same text the deltas
carry, and it is the one the API considers canonical.

## tests::reasoning_deltas_are_kept_without_an_added_event

Some providers stream deltas without an `output_item.added` first. The summary
must survive that too.

## tests::retry_delays_double_and_then_stop_growing

Past the cap the wait stays the same rather than becoming a silence of its own.

## tests::a_silent_response_is_retried_but_a_spoken_one_is_not

A request that said nothing is worth asking again; one that said something is
not, because the retry would answer a second time.

`0` is "no ceiling", not "no retries": a ten-day turn must not die on a
transient outage, and the cancel token is what stops it. Even unlimited retries
never ask twice after the model has spoken.

## tests::a_thinking_level_rides_on_the_request

`set_session --thinking-effort` reaches the wire as the Responses API spells it,
and a session that never asked sends nothing at all -- an empty `reasoning`
object is not the same as no reasoning to a provider.
