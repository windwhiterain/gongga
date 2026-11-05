# `crates/server/src/anthropic.rs`

## why a second face exists

`docs/reference/prefix-cache.md` says what each provider sells. What was measured here on
2026-11-05 is narrower and decided the question: on MiniMax's OpenAI-shaped faces, a
request is served from cache **only when the provider has already been sent that prompt**.
A new prompt answers 128–192 cached tokens however much of its head another prompt
already sent:

| send (a real 19,422-token body, `/v1/responses`) | cached |
| --- | --- |
| a prompt sent the day before | 19,421 |
| the same again | 19,421 |
| a sibling session's first request, *only* the card renamed | **0** |
| the same, one character of the user message changed | 128 |
| that sibling again | 19,200 |

So the head work was right and could not pay: the head was already byte-identical across
sessions (tools identical, 12,448 characters of common instructions) and it bought
nothing. DeepSeek, for contrast, answered 15,360 of 15,625 to a sibling's *first* turn —
it does serve a prefix.

MiniMax's `/anthropic` face does too: the same 8,260-token system text with a
`cache_control` breakpoint answered 8,192 to a request whose user message had changed,
and 8,266 to one with a sentence appended. That is a prefix cache, and it is the one
lever measured to reach a session that has never sent anything.

**The price of it is a second request shape.** The face disagrees with the Responses API
about where the system text goes, how tools are spelled, what a tool result is, and where
usage is reported. Half of each is not a request, so a provider is spoken in exactly one
face per request, chosen by the roster (below).

## what the face asks for, measured on the endpoint

| fact | what MiniMax's face does |
| --- | --- |
| `max_tokens` | **required**; the ceiling is 524,288 for both models this roster lists (`>524288` answers 400, code 2013) |
| `system` | a block list; `cache_control: {"type": "ephemeral"}` marks the end of the cacheable prefix |
| `tools` | `{name, description, input_schema}` |
| tool result | a `user` message carrying a `tool_result` block with `tool_use_id` |
| consecutive `user` messages | accepted, so a row needs no merging pass |
| `output_config.effort` | accepted, and it is the spelling the roster records for this face |
| images | `{type:"image", source:{type:"base64", media_type, data}}` |
| usage | `message_start` and `message_delta`: `input_tokens`, `output_tokens`, `cache_read_input_tokens`, `cache_creation_input_tokens` |
| stream | `message_start`, `content_block_start/delta/stop`, `message_delta`, `message_stop`, `ping`, `error` |

`input_tokens` excludes the cached part on this face, so [`crate::anthropic::usage`]
adds the three back into the one number the rest of the server reads; a measurement that
took only `input_tokens` would price the request as if the cached part had never been
sent.

## the request, and why nothing is stored in this shape

A row keeps the shape it was appended in — the transcript is append-only and a
provider's protocol is not a reason to rewrite one. So the stored Responses item is
**converted on the way out**: `function_call` becomes `assistant`/`tool_use`,
`function_call_output` becomes `user`/`tool_result`, a text row becomes a `text` block,
and a row this face cannot send (a reasoning item, a provider-side tool item) is left out
of the request and stays in the transcript.

That is the one place `body.rs`'s "a row is copied as stored" no longer holds, and it
holds per face rather than per row: the Responses face still copies.

## two breakpoints, and what they are worth

A breakpoint marks the end of what a provider may reuse. Two are sent:

1. **the system text** (the prose only), which every session of every directory sends
   identically -- the index travels as a message, so it cannot change the system text;
2. **the message before the newest one**, which the body holds back until another
   arrives. The newest message is never marked: it has never been sent, and a breakpoint
   behind it caches a prefix nothing can reuse.

**They are worth nothing measurable on this provider, and the reason corrects an earlier
reading.** `cache_creation_input_tokens` was 0 on every request that carried one, and a
prompt whose bytes a stored prompt does *not* prefix answers 128–192 however much of its
head was already sent. What this provider serves is one thing: a request that **extends**
a stored request — the same conversation, one step later — and an exact repeat. See
`docs/reference/prefix-cache.md`, "the boundary this provider actually has".

The 15,616-of-15,640 readings first read as a sibling's first turn were that session's
*second* request inside its own turn (a tool loop extends the prompt, so the first request
is stored and the next one extends it). A session's `cached_tokens` is the newest
measurement it has, so a one-request turn looks identical to a two-request one in that
column unless the rows are read too.

## which provider is spoken in which face

The roster owns it, because it is a fact about that provider:
`[[provider]] api = "anthropic-messages"` names an endpoint the provider declares. A name
the provider does not offer falls back to the built-in order (`openai-responses` first)
rather than failing: the roster is read, not obeyed. A home may override the choice with
`[model] api`, for a machine that knows better.

`max_tokens` comes from the model's own `max_output_tokens` in the roster (a measured
ceiling, not a budget this server invented) or from `[model] max_output_tokens`. With
neither, a request in this face is refused with a message naming both places: a guessed
answer length is worse than a refusal.

## what is not here

- **No `thinking` replay.** This face returns thinking blocks and this server stores a
  reasoning row per provider item in the *Responses* shape; sending those on this face
  would be sending one protocol's item in another's. They are dropped on the way out, and
  the probe round trip (tool call, tool result, second request) is accepted without them.
- **No file uploads.** A picture travels inline as base64 on this face: there is no
  Files API behind it, and the bytes this home keeps are the same bytes the other face
  would have uploaded. `Files::base64_part` is that path.
- **No `store`, no `include`.** This face has no server-side transcript: the history is
  what the request carries, which is what this server sends anyway.
