# A tool result streams its channel

2026-11-23, session `result_streaming`. The user: tool results do not stream — a
compile channel's log should stream in the UI, not arrive whole at the end.

## What was already there

SPEC's UI section already lists `tool_result: channel's output streaming`, and the
whole road existed: `ToolContext::events` -> the agent's forwarder -> `Event::ToolAnswer`
-> the client's one `t:<call_id>` block -> the TUI journal. The agent even had the
rule "a tool that streamed nothing owes its output to the final chunk" (a count of
what the forwarder published). **Nothing fed the sink.** When `bash` was retired for
channels, the streaming went with it and no tool ever sent a chunk again.

## The change

`Channels::talk` takes an optional `Tail` (the call's sink + `call_id`); `exec` builds
it from `ctx.events`, the `input_channel` server cmd passes none. While the call waits,
each poll publishes the newly-arrived whole lines; when it answers, the remainder goes
out before the verdict. No client or TUI change: the chunks ride the path an assistant
answer already streams by.

## Two decisions worth keeping

- **The stream is a view, never a read.** It moves no cursor, so the model's answer is
  byte-for-byte what it was before — a timed-out call still answers empty and leaves the
  window for the watcher's notice. Only the reader gains the live log.
- **Show whole lines only (`sayable`).** A partial line may still be the marker's; holding
  it (and a line break that ends the window) means a split sentinel is never drawn and the
  cut at the marker cannot retract what was shown. The streamed body and the answer end up
  the same bytes.

## Measured

L3 on the worktree build, fake LLM calling `exec` on a program printing a line a second:
three `done:false` chunks at 1.03s / 2.05s / 3.08s (`line-1`, `\nline-2`, `\nline-3`), the
held trailing `\n` at program end, then one `done:true` verdict with an empty body.
`.dev/verify/<sha>-result-streaming.json`.
