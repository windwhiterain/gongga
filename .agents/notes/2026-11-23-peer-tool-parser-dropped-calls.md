# The peer's tool parser dropped every call the harness made

2026-11-23, user report: in session `probe` (model `kvmask-peer/minicpm5-2b`) the
assistant reasoned about calling a tool and then made no call at all.

## What the transcript showed

`probe` (session id 431), turn 2: `messages` held the `reasoning` row and nothing else —
no `tool_call`, no tool result, no assistant text. `tool_calls` had no row for the
session. The turn's own event said `outcome=done, steps=0, answer=null`, and its
`llm_attempt` said `output_deltas=1, output_chars=2, tool_items=0,
tool_argument_deltas=0`. So the harness stored everything the provider sent; the call
was never in the stream.

## Cause, from the peer's own log

`~/kv-mask/exp/minicpm-serve.log`:

```
ERROR [minicpm5xml_tool_parser.py:788] AttributeError: 'FunctionTool' object has no attribute 'function'
ERROR   line 272, in _build_tool_map    name = tool.function.name
ERROR Error in MiniCPM5XMLToolParser.extract_tool_calls_streaming
```

`_build_tool_maps` read the Chat Completions tool shape (`tool.function.name`,
`tool.function.parameters`). The harness speaks the Responses API, where a tool is a
`FunctionTool` carrying `name` and `parameters` directly. vLLM caught the exception,
logged it and dropped the call, so the model's two-character tail arrived as plain text.

## Fix

`~/kv-mask/exp/apply_tool_shape.py` (idempotent) rewrites that loop to read either shape:
`getattr(tool, "function", None)` first, else `tool.name` / `tool.parameters`. The file it
patches is in `.venv-vllm/lib/python3.12/site-packages/vllm/tool_parsers/`, so **re-run the
script after any vLLM reinstall**. The endpoint then needs a restart
(`systemctl --user restart minicpm-endpoint.service`).

## Verification (2026-11-23)

A session of this build's own (`mask-offload/repro`, deleted afterwards) sent the same
message the probe got, twice.

- Before the patch: 1 step, `outcome=done`, `output_chars=2`, `tool_items=0`, reasoning
  row only — the reported symptom reproduced exactly.
- After the patch and a service restart: turn 2 ran **5 steps** and stored 5 `tool_call`
  rows with their results (`ls_channel`, `exec` bin=cmd, `gongga_cli`, `exec` bin=cmd,
  `gongga_cli`), two of them refused by the harness for their own reasons and still
  recorded. Nothing was dropped.

The model's tool craft is poor (it reached for `cmd`, then `gongga_cli` with an unknown
command `ls`), but every call it made arrived.

# Second bug in the same turn: no context was ever reported

2026-11-23, user report: "推理服务怎么没报 ctx".

## Symptom

Session `probe` kept `context_tokens = 0` and `tape_tokens = NULL` although every request
returned usage, and its transcript had no `usage` event at all. The runtime log line
`provider usage session=probe input=... output=...` was never printed for those turns.

## Cause

The endpoint answered with

```json
"gongga_unmasked_input_tokens": null, "gongga_memory_tokens": null
```

because both come from `getattr(request, "_gongga_view", {}).get(...)`, and a request
carrying no mask view has no such attribute. The harness deserializes `usage` into
`Usage { gongga_memory_tokens: Vec<MemoryTokenUsage> }`; `#[serde(default)]` covers an
*absent* field, not a `null` one, so the whole object failed to deserialize and
`serde_json::from_value(..).ok()` turned the entire measurement into `None` — no usage
event, no `context_tokens`, no cached/tps/ttft from the provider's counts.

## Fix

The server patch now reports the visible count and an empty list rather than nulls:
when no mask view is present, every input token is visible, so
`gongga_unmasked_input_tokens = num_prompt_tokens` and `gongga_memory_tokens = []`.
Changed in `apply_server_patch.py` (the text it inserts) and re-applied from the
`.kvmask-orig` backups, so a reinstall gets the same code.

## Verification (2026-11-23)

- Raw endpoint usage now: `input_tokens=12, gongga_unmasked_input_tokens=12,
  gongga_memory_tokens=[]`.
- A session of this build's own (`mask-offload/ctx`, deleted afterwards) sent one short
  message: `context_tokens = 24560`, `tape_tokens = 24560`, `cached_tokens = 0`,
  `tps = 25.16`, `ttft_ms = 9288`, and a `usage` event reading
  `input=24560, unmasked_input=24560, cache_write=24512`.

## Still open

The harness loses an entire usage record to one unparsable field, silently (`.ok()` in
`llm.rs`, the `response.completed` arm). A provider that sends `null` for a list costs
the session its whole context display instead of one field.

# The harness side, fixed (`84bcbbeb`)

2026-11-23, user approved tolerating `null` there and saying so in the log.

- `Usage.gongga_memory_tokens` now reads through `null_as_empty`: a `null` list is an
  empty one, and the counts beside it survive.
- The `response.completed` arm no longer hides a parse failure in `.ok()`: a usage that is
  present but unreadable logs `the provider's usage did not parse` once, and the stream
  still completes (the counts are lost, not the answer). An absent (`null`) usage stays
  silent, which is what a provider declining to report looks like.

Verification (same day, one server in a home of its own and a fake provider, both gone
afterwards): a fake that completes a response whose usage spells
`gongga_memory_tokens: null` stored `context_tokens = 40, tape_tokens = 40`; the next
turn's usage spelling `input_tokens: "forty"` logged exactly one warning, ended the turn
anyway, and the session kept its last known count.

One gotcha found on the way: `mbx build` rebuilt `gongga-server`'s library but did not
relink `target/debug/gongga-server.exe` (it stayed an hour old, and the first run of the
check above measured the stale binary and read zeros). `mbx build -p gongga-server` did
relink it. Check the binary's timestamp before trusting a run.
