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
