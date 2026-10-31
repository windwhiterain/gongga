# OpenAI Responses API

The shape this server speaks (`POST {base_url}/responses`), as the official spec
defines it. Not what we chose: what we must fit.

Source: [`openai/openai-openapi`](https://github.com/openai/openai-openapi)
`openapi.yaml`, `info.version: 2.3.0`, fetched 2026-10-31. Component names below
are the spec's own, so a claim here can be checked without reading this file.

## The request

`POST /responses`, body `CreateResponse`. The two fields this server builds by
hand are worth naming:

- `input` — `InputParam`: either a string, or **an array of items** (`InputItem`,
  a `oneOf` discriminated by `type`). The array is a transcript, not a script:
  messages, completed assistant items (reasoning, function calls), and the
  results of those calls all sit in it.
- `include` — `IncludeEnum`. `reasoning.encrypted_content` is documented as
  "an encrypted version of reasoning tokens in reasoning item outputs. This
  enables reasoning items to be used in multi-turn conversations when using the
  Responses API statelessly (like when the `store` parameter is set to `false`)".

For streaming, the same description says to take the encrypted content **from
`response.output_item.done`**: the copy in `response.output_item.added` may be
incomplete. A server that keeps the first and drops the second replays garbage.

## The items this server sends

### `reasoning` — `ReasoningItem`

> A description of the chain of thought used by a reasoning model while
> generating a response. Be sure to include these items in your `input` to the
> Responses API for subsequent turns of a conversation.

`required: [id, summary, type]`. Fields:

| field | type | meaning |
| --- | --- | --- |
| `type` | `"reasoning"` | constant |
| `id` | string | "The unique identifier of the reasoning content" — **required** |
| `summary` | `SummaryTextContent[]` | `{type: "summary_text", text}` — the reader's summary |
| `content` | `ReasoningTextContent[]` | `{type: "reasoning_text", text}` — the thinking itself |
| `encrypted_content` | string \| null | the opaque blob, populated by default on `/v1/responses` |
| `status` | enum | `in_progress` / `completed` / `incomplete`, populated when returned |

`summary` is required and may be an empty array. `content` is optional and is the
only field that carries the reasoning as plain text.

### `function_call` and `function_call_output`

`FunctionToolCall` (`type: function_call`, `call_id`, `name`, `arguments`) and
`FunctionCallOutputItemParam` (`type: function_call_output`, `call_id`,
`output`). They are matched **by `call_id`**, which is the spec's only statement
about how a call finds its result. Nothing in the schema requires the result to
be the array item directly after the call, and nothing forbids two calls
standing together: `parallel_tool_calls` is a first-class request field, so one
step routinely produces several `function_call` items.

## What the spec does not say

Checked against the same document, because these were assumed here once:

- No rule that a reasoning item must be repeated before each call of a step.
- No rule that `function_call_output` must directly follow its `function_call`.
- No rule that a reasoning item must precede the calls it led to.

Those are provider behaviour, not the contract. See the measurements below.

## Measured: DeepSeek's Responses endpoint

Same request body the server builds, against `api.deepseek.com/v1/responses`,
`deepseek-flash`, `reasoning.effort: high`, `store: false`, one step that asked
for two tools. The 400 is always the same sentence: *"The `reasoning_text` in
the thinking mode must be passed back to the API."*

| items for the step | result |
| --- | --- |
| `reasoning, call, output, call, output` | 400 |
| `reasoning, call, call, output, output` | 200 |
| `reasoning, call, call, output, output` (outputs in completion order) | 200 |
| `call, reasoning, call, output, output` | 400 |
| `reasoning` with its encrypted blob and no `content` | 200 |
| no reasoning item at all | 400 |

So on this provider the sentence is a lie about its own cause: what it rejects is
the *order*, not a missing text. A reasoning item has to come before the calls of
its step, and the calls have to come before the outputs — which is exactly the
order the API itself returned them in. Interleaving one call's output between two
calls is what produces the reasoning_text error, and it is what this server used
to do: `Step::flush` pulls each result up beside its own call.

Sending the reasoning text back is harmless (the spec allows it) but unnecessary
on this provider once the order is right.

## What the server does about it

- Each `tool_call` and `tool_result` row records the model step that wrote it, so
  the request builder can tell "one response asked for two tools" from "two steps
  asked for one each". Without that number the rows are indistinguishable, and
  the builder used to write one call's answer between two calls.
- `Step::flush` writes a step as reasoning, then calls, then results.
- The builder keeps a whole step and writes it when a row of another step
  arrives, instead of the moment the last open call is answered.

## Not established

- Whether OpenAI's own endpoint accepts the same grouped order. It follows from
  the schema's silence and `parallel_tool_calls`, but no key was available to
  measure it. If a provider ever refuses grouped calls, this table is where the
  evidence should be added.
- Any provider other than DeepSeek.
