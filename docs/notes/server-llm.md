# server/llm.rs

## llm

Streaming client for the OpenAI Responses API.

The wire format is server-sent events over a POST to `{base_url}/responses`.
We parse it by hand rather than pulling in an SSE crate: the event set we
care about is small, and owning the parser means we can surface partial
tool-call arguments to the UI as they arrive.

## LlmClient::new

The client carries no deadline of its own. A client-wide timeout covers the
body as well as the answer, and a body that is still producing is exactly what
a long answer is entitled to do -- so the bounds sit where each one knows what
it bounds: `CONNECT_TIMEOUT` on the socket, `headers_timeout` on the answer's
headers, `idle_timeout` on the content.

## LlmClient::model

The model this client asks for.

Logged at the start of every turn, so "which model answered this one" is a
fact rather than something to infer from the transcript.

## LlmClient::stream

`handover`, when present, stands in for the part of the transcript that has
been folded out; it is sent as the first item so the model reads it
before anything else.

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

## LlmClient::build_body

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

`produced` is the only thing that rules one out: once the model has said
anything, asking again would stand a second, different answer behind the first.
Everything else -- an error, a timeout, a completion with nothing in it -- is
the provider's bad minute and deserves another try.

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

## LlmClient::build_body — one `tools` array

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

If that shows up: `[llm] store = true` is already a knob here, and the shape to
send becomes `previous_response_id` plus only the new items, which is a change to
`build_body` and to what `store` is allowed to hold. See
`docs/notes/server-store.md` for why the item is stored verbatim in the first
place.

## image_content

The one place a stored picture becomes bytes. A row keeps a path and a media
type; the file is read when the request is built, and encoded as a data URL in an
`input_image` part inside the `function_call_output`'s content array.

When the file is gone, the part degrades to an `input_text` saying which path is
missing and that what it showed cannot be recovered. A turn that has lost a
screenshot is still a turn; failing the whole request over one deleted temp file
would make the session unusable until someone recreated it.

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

The tool schema is not part of the transcript, so nothing a fold does can
remove it -- but that also means nothing would notice if the body stopped
carrying it. It is rebuilt from the registry every request, and this pins that.

A folded prefix must not change what the model is offered: the schema is out
of band, not part of the history.

An empty registry means no `tools` key at all, rather than an empty array that
some providers reject.

## tests::a_provider_payload_round_trips_through_the_blob_directory

Three claims in one test: the row holds no base64, the reassembled item is
byte-equal to the one that arrived, and with no directory to read from the item
is refused rather than sent short. The refusal keeps its `type` and `id` in the
log line, because "an item was dropped" is not something anyone can act on.

`storable_activity_with` is the same write with the directory named by the test,
so no test depends on which home the process was started with -- the global is
set once per process, and a test that leaned on it would be order-dependent.

## tests::builtin_tools_share_the_array_with_functions

The two kinds of tool land in one `tools` array, in the roster's own spelling,
with the local ones first. This is the shape a provider reads, so it is the shape
the test pins.

## tests::a_model_that_declares_no_builtin_tools_sends_none

Undeclared is unsent, and no tools at all is no `tools` key rather than an empty
array.

## tests::a_provider_tool_item_is_parsed_not_swallowed

The regression this whole section exists for: a `web_search_call` used to end in
`ItemKind::Unknown`. The test drives the three SSE events a real one sends and
asserts the activity line, the query in it, and the replayable item behind it.

## tests::the_suffix_not_a_list_of_names_decides_what_is_a_provider_tool

A provider this server has never heard of still gets a visible line.

## tests::an_activity_item_replays_exactly_as_it_came

Verbatim replay of an item that never had a payload (a search), and the same
refusal for one marked non-replayable.

## tests::a_picture_rides_in_the_tool_result_as_input_image

The content array shape: `input_text` first, `input_image` second, a data URL --
not a base64 sentence for the model to parse.

## tests::a_missing_picture_degrades_to_a_sentence

The degradation path, named path and all.

## tests::base64_matches_the_standard_alphabet

Standard alphabet, padded, including the byte values a hand-rolled table usually
gets wrong. A data URL with the wrong encoding is a 400 nobody can read.

## tests::a_text_only_model_reports_why_it_cannot_look

The roster's `modalities` decides, and the refusal names the entry and the
missing modality; unstated means nothing is refused on the roster's behalf.

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

## tests::a_refused_request_is_not_retried

A 4xx that is not about timing is a refusal: `400`, `401`, `403`, `404`, `413`
and `422` all end the turn. `408`, `429` and the 5xx family stay retryable,
because they describe the provider's minute rather than the request.

## tests::a_retry_names_the_previous_attempt

The reported failure wins; the three silent shapes each get their own sentence.

## tests::a_streamed_error_decides_by_its_kind

`invalid_request_error` and `context_length_exceeded` are refusals;
`server_error` is a bad minute; an unknown kind keeps the try it might deserve.

## tests::a_size_refusal_says_how_to_fold

The provider says "too long" and stops there. The message gains the command
that folds the transcript, and nothing else gains anything.

## tests::a_thinking_level_rides_on_the_request

`set_session --thinking-effort` reaches the wire as the Responses API spells it,
and a session that never asked sends nothing at all -- an empty `reasoning`
object is not the same as no reasoning to a provider.
