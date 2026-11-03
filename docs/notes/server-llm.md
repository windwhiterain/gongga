# server/llm.rs

## llm

Streaming client for the OpenAI Responses API.

The wire format is server-sent events over a POST to `{base_url}/responses`.
We parse it by hand rather than pulling in an SSE crate: the event set we
care about is small, and owning the parser means we can surface partial
tool-call arguments to the UI as they arrive.

## LlmClient::new

Every request to a provider says what is calling: the client sets
`User-Agent: gongga-harness/<version>` once, at construction, so no call site
adds it and no request can forget it. A provider log or a proxy on a shared key
can then attribute the traffic to this harness instead of to an anonymous client.
The version is the crate's, which is what a released binary reports anyway.

The client carries no deadline of its own. A client-wide timeout covers the
body as well as the answer, and a body that is still producing is exactly what
a long answer is entitled to do -- so the bounds sit where each one knows what
it bounds: `CONNECT_TIMEOUT` on the socket, `headers_timeout` on the answer's
headers, `idle_timeout` on the content.

## LlmClient::model

The model this client asks for, `provider/model_name`.

Logged at the start of every turn, so "which model answered this one" is a
fact rather than something to infer from the transcript. The request's own `model`
field carries the other half -- the name alone -- because a provider does not take
its own id back.

## LlmClient::stream

The request's own items are the transcript's rows; the start card, when there is
one, is the only item built here rather than read from a row.

A request that fails, times out, or completes with nothing in it is retried
with a doubling delay, because all three look identical from here and all
three are the provider's bad minute rather than ours. The caller sees one
stream either way; a retry that is about to happen is announced as
`LlmEvent::Retrying` so the operator can watch it happen.

An empty completion is worth asking for again: the provider acknowledged the
request and then said nothing, which is what an overloaded or truncated
response looks like. So is a stream that stopped without completing.

A *refusal* is not asked again. The event carries `retryable`, and a 4xx other
than `408`/`429` -- or a streamed `invalid_request_error`,
`context_length_exceeded`, and the like -- says the request itself is wrong, so
every retry would get the same answer, forever, with `max_retries == 0`. That
is the shape a too-long transcript used to take: an endless "retrying" warning
and no failure, so nothing reached the operator. It now fails the turn at once
with the provider's words, and a size refusal gains the one instruction the
provider leaves out -- the `transcript.py collapse` command that makes it fit.
An unknown streamed error kind stays retryable: providers invent their own
spellings, and giving up on one this code has not heard of is worse than one
extra try.

A panic is one attempt's failure, not the stream's end. Reading a response runs
inside `catch_unwind`, so a beat this code cannot survive arrives as a retryable
failure rather than unwinding past the loop. Before that, a panicking read
dropped the event channel, the caller saw a stream that had merely stopped, and
the turn ended in `Error` with no retry and no reason for either. A stream that
ends with neither a completion nor a failure reports itself the same way.
Whether it gets tried again is still the produced rule: text already forwarded
cannot be unsaid, so an attempt that spoke keeps its partial answer and fails.
Silence is a different question with a different answer: `read_stream` moves its
idle clock only when the attempt *said* something (`Outbox`), never when the
socket merely carried bytes, so a long answer is cut by nothing but its own
silence, and a keep-alive cannot hold a turn open.

## AttemptProbe and the `llm_attempt` event

Every HTTP attempt writes one aggregated event after its last response event,
including failed and cancelled attempts. Deltas are counted in memory rather
than appended to SQLite one by one, so instrumentation does not turn into the
I/O bottleneck it is meant to measure.

The payload separates request-head construction and transcript opening from the
network attempt. It carries `started_at`/`ended_at`, outcome and HTTP status,
headers and first/last reasoning, output and tool-argument delta times, and
`effort` -- the reasoning level the request carried, so "the title showed one level
and the request sent another" is a question the rows answer. A summary delivered
whole in its done event counts as reasoning characters too, or a run with no deltas
reads as a run that thought nothing. Raw
chunks and bytes, SSE event kinds, tail events and parse errors make the actual
wire shape inspectable without storing another copy of the response.

`process_total_us` and `process_max_us` measure JSON parsing, event handling and
sending into the bounded LLM event channel. They are local work, not provider
generation time. Comparing them with attempt wall time identifies local parse
or backpressure overhead; tool execution is measured separately because tools
run while the response is still streaming.

The original provider `usage` object is included verbatim. Optional token
details such as `output_tokens_details.reasoning_tokens` therefore survive when
the provider supplies them, while character and delta counts remain available
when it does not. Characters are never presented as estimated tokens.

## LlmClient::build_head

The Responses API's spelling for "how hard to think", which the roster records
per model as `reasoning_field.responses`. It is the only surface this server
speaks, so there is nothing to switch on.

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

How far the attempt got is *not* part of the question. It used to be: an attempt
that had streamed anything was never resent, on the theory that asking again
would stand a second answer behind the first. That theory cost a whole turn to a
provider that died six characters into its answer, and the six characters were
never an answer -- they were a fragment the transcript does not keep, because
`Agent::step_inner` discards what the failed attempt wrote before the retry is
sent (see `server-agent.md`). A provider that dies mid-sentence is the same bad
minute as one that dies before its first byte, and the user's rule is explicit:
retry always, unless the provider says the request itself is invalid -- which is
`error_retryable` and `status_retryable`, not this function.

`max_retries == 0` is *no ceiling*: a long-running turn is not allowed to die on
a transient outage at hour three, so the default retries until the turn's cancel
token fires. The cancel is the only honest stop -- nothing about a bad
provider's recovery time is knowable in advance.

## LlmEvent::Retrying

Carries `reason`, which is the *previous* attempt's own words. It used to carry
only how many and how long: the first failure of a request is the only clue to
what is going wrong, and once a retry answers, nothing else records it. The
agent prints it; the `tracing` line prints it too, so a retry storm in the
server's stdout says what it is retrying rather than only that it retried.

## retry_reason

The words for a `Retrying`. A failure the attempt reported keeps its own text;
otherwise the reason is read off what the attempt actually did -- completed with
nothing in it, spoke and stopped before completing, or ended with nothing at
all. Never empty, and there is a test for each of the three.

## status_retryable / error_retryable / with_size_hint

The three parts of "was that a bad minute or a refusal": an HTTP status, the
kind a streamed `error` event names, and the sentence a size refusal needs
added because the provider stops at "too long".

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

## LlmClient::builtin_tools

Provider-side tools, taken from the roster's `builtin_tools` for the model the
session is on and sent **verbatim** as `{"type": "<name>"}`.

No mapping table lives here on purpose. OpenAI calls it `web_search` in the
Responses API and `web_search_preview` elsewhere, and a provider may have its own
`web_fetch`; a table would freeze "what a provider can do" into this server,
which cannot know what a provider it has never talked to accepts. The roster is
where that vocabulary belongs, because the roster is the file a human edits when
a provider changes.

Undeclared means unsent. Offering a tool a provider does not recognise is a
plain 400 on every request of every turn, so the default has to be silence.

## LlmClient::build_head — one `tools` array

Local functions and provider tools share a single array: the API has one
`tools` field, and splitting it would mean inventing a second key no provider
reads. Nothing distinguishes them there except their own `type` -- `function`
for ours, the tool's own name for the provider's.

The key is omitted entirely when both are empty, because an empty array is not
the same as no `tools` key to some providers.

## handle — provider-side tool items

`web_search_call`, `web_fetch_call`, and whatever a provider invents next
(`*_call`) used to fall into `ItemKind::Unknown` and be dropped: the operator saw
a turn that went quiet while the model was reading the web.

The rule is the suffix, not a list of names -- a name this server has never heard
of must still be visible. `function_call` is matched first, because it is ours
and needs an output; a provider item was already executed where it ran and owes
nothing.

Each one becomes an `Activity` response part: the line opens with the tool's name
(`web_search`, `web_fetch`) and gains a tail from the item itself -- the query,
the URL, the generation prompt -- plus the provider's own progress beats
(`searching`, `in_progress`, `generating`) as they arrive, on the same stream
key. The front-end renders it as `[session] provider: ...`, distinct from
`[session] tool: ...`, because the two mean different things: one this harness
ran, one it did not.

## LlmEvent::Citation

`response.output_text.annotation.added` carries a `url_citation`: the source
behind a sentence. It becomes its own `Activity` line and an operator-only
`notice` row. A search whose citations nobody can see is unverifiable -- the
answer looks the same whether the source exists or the model invented it.

## LlmEvent::BuiltinCall

The provider's own item, kept verbatim (minus any binary payload) so the next
request can send it back.

This is the part that is easy to get wrong. A `function_call` is paired with a
`function_call_output` we produce; a `web_search_call` has no local output,
because the provider already ran it. Replaying it means putting *its own item*
back in `input[]`, exactly as it came. Dropping it rejects the transcript -- the
API sees a turn that mentions a search nobody performed -- and so does replaying
one that lost its payload.

`image_generation_call` carries a base64 `result`. The bytes go to a file beside
the home and the row keeps `result_blob`, so the transcript stays text and small
*and* the item can be sent again: see `docs/notes/server-blobs.md`. What that
replaced -- keep the item, drop the payload, warn at send time -- was cheaper to
build and wrong to live with: a session that generated one picture could not then
carry it in its own history, and every later request was silently shorter than the
one before it.

## 待确认：does every provider take its own `*_call` item back?

Open risk, not verified against a real provider. Replaying an activity item
verbatim assumes the provider accepts an item **it produced** appearing in
`input[]`. OpenAI's Responses API does, when the transcript is self-contained.
A provider that keeps the state on its side instead -- the ones that expect
`store: true` and a `previous_response_id` to continue a conversation -- may
reject it: most likely one HTTP 400 with an `invalid_request_error` naming the
item or the missing `previous_response_id`, on **every** request of a session
whose transcript holds one such item, which would look like a permanently broken
session rather than a bad item.

If that shows up: `[provider] store = true` is already a knob here, and the shape to
send becomes `previous_response_id` plus only the new items, which is a change to
`build_head` and to what `store` is allowed to hold. See
`docs/notes/server-store.md` for why the item is stored verbatim in the first
place.

## the picture part: `image::files`

The one place a copy becomes a request part, called from `body.rs` while the request is
written. A row keeps a path, a media type and the id of the copy in this home; the file
the provider holds *now* wins when one can be had, the copy is read (and uploaded once)
when it cannot, and the path is the last resort for a row written before copies existed.
When nothing can be read either, the part degrades to an `input_text` naming the path. A
turn that has lost a screenshot is still a turn; failing the whole request over one
deleted temp file would make the session unusable until someone recreated it.

The provider's id is looked up rather than stored because a provider deletes its files,
and a row recording one would be a row that can never be sent again. `Files` memoises
what it resolved for the request, so one picture named ten times is one upload.

## ItemMeta::text

Text accumulated from deltas. Providers do not all return the finished text on
`output_item.done` (MiniMax sends an empty `summary` array), so the deltas are
the only place the readable text ever exists.

## Usage, and the half of it that matters for cost

`usage.input_tokens_details.cached_tokens` is the provider's own answer to a
question this process cannot ask any other way: was this request's prefix the same
as the one before it? The struct keeps the nested shape it arrives in and exposes
`cached_tokens()`; `None` means the provider declined to say, and is deliberately
not turned into a zero -- a fabricated miss reads exactly like a real one in a log,
and the two have opposite meanings.

It is worth reading because this harness re-renders its prose before every request.
`instructions` sits in front of the whole transcript in the cached prefix, so an
edit to `[prompts]` costs the history behind it. Measured: a session warm at 99% dropped
to 35% on a one-sentence edit -- the tool schemas ahead of it stayed cached, the
conversation behind it did not -- and stayed at 35% after the sentence was put back,
because the next request has to match the request that carried the edit.

Walking into another directory does not cost it any more. The row still records where
the shell stood, but nothing is read off disk into the prose, so the prefix bytes are
the same on either side of a move.

The measurement is `scripts/fake_llm.py`, which models a hit rather than inventing
one: it splits the body into the units a prefix sees (tools, instructions, then each
input item), compares them with the previous request's, and reports the shared
prefix as `cached_tokens`. "We did not break the prefix" is then a number, not a
reading of code.

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

## handle (reasoning summary done)

`response.reasoning_summary_text.done` and `response.reasoning_text.done` carry the
whole summary in one event, which is sometimes the only copy: Kimi's endpoint streams
`reasoning_summary_text.done` with no deltas at all in most attempts, and its
`output_item.done` carries an empty `summary` array. Ignoring the done event left every
such step with no reasoning text: nothing to draw, and a stored entry whose summary was
the empty string -- which read, from the front-end, as a model that had not thought.

The done text is the full text by definition, so the item's accumulated text is its
prefix and only the remainder is news: one rule covers a provider that sends both
(nothing emitted) and one that sends only this (the whole text emitted as one delta).
A provider whose done text contradicts its deltas keeps what it streamed rather than
showing a second copy.

## handle (output_item.done)

The readable summary is the provider's if it sends one; otherwise the deltas we
accumulated are all there is. MiniMax is in the second camp: its `summary` array
on `output_item.done` is empty, so without this every stored reasoning entry
would have a blank summary forever.

A `summary` array is a list of *paragraphs* -- one `summary_text` per part -- so the
parts are joined with a newline. Joining them with nothing glued a multi-part summary
into a single line, which is how a reader lost every break the model made; the
front-end then had one row to draw where the model wrote several.

## history_item_to_input

A reasoning row replays as the thought it was, and what goes on the wire depends on
what the row holds.

With the encrypted blob, that is the whole item: the blob *is* the thought, so the
`summary` beside it stays off the request. Echoing both would put every thought into
`input[]` twice, which is a request the provider charges for and does not need.

Without a blob -- a thought a cancel interrupted, kept by `keep_streamed` -- the text
is all there is, so it is replayed as `summary`. Dropping it instead was the older
rule, and it made a cancel lose the half of the turn the reader had already watched.

## Comments moved out of the code

The checker holds a preamble to 120 characters and an `# Invariant` to 400, so the
argument behind a short claim lives here.

**Three bounds, knowingly.** There is no client-wide deadline on purpose: a wall clock
would cover the body too, and a body still producing is the one thing an answer of any
length is entitled to do. What is left needs bounds that know what they are bounding:
`CONNECT_TIMEOUT` for the socket, `headers_timeout` for the answer, `idle_timeout` for
the content. The identity is the client's, not one request's -- every call to a provider
goes through this client.

**`idle_timeout` measures content, not bytes.** Chunks that carry no event (keep-alives,
comments) do not move the clock, so a provider holding the socket open with nothing to
say is as silent as one sending no bytes at all. It is deliberately not "time since the
request started", which would cut a long answer in the middle of writing it.

**A dead file id.** Answered once by forgetting the id and sending again, and *not*
through `retryable`: that counts attempts against a configured ceiling, and this failure
is not a bad minute -- its fix is not another try of the same bytes.

**The stream's invariant, in full.** A failed, timed-out, or empty attempt is retried
with a doubling delay: the caller always sees one stream, and each retry is announced as
`LlmEvent::Retrying`. A panic inside one attempt is that attempt's failure and enters
the same loop, so it never ends this stream by unwinding past it. How far an attempt got
is not a reason to stop: only the provider saying the request itself is wrong ends this
stream.

**Two lifetimes in `build_head`.** The index joins the prose because both change only
when a file does. The card does not: the prose follows an edited template while the card
is stamped once.
