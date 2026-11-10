# A waiting call returns early, and nothing is named "cut"

SPEC's `interrupt_session` now reads:

```python
fn interrupt_session(id: Session):
  # for waiting tool call, make it early return.
  # stop llm streaming and loop, everything received must be saved to context.
```

The word that says what happens is **early return**. There is no `cut` in the SPEC, and
there is none in the code any more.

## What was there

`waited()`, the one wait in `ToolRegistry::run`, spawned the work on a task, watched the
turn's token and the inbox, and on either one did `task.abort()` and then asked the tool
`Tool::cut(args, ctx, why, started)` what it had left. That hook carried `Cut::{Message,
Interrupted, Cancelled}` so the dropped body's aftermath could be worded, and it produced
sentences no SPEC asked for: "`bash` ended its wait: a message arrived for this session.
What it had already done is not reported...", "the session was interrupted: this wait ends
here", "`interrupt_session` ended this wait: the turn stops with this answer".

Three separate inventions, one root: a body that had already been dropped cannot answer,
so a hook was invented to answer for it — and then the hook needed vocabulary, and the
vocabulary leaked into what the model reads.

## What is there

The body is not dropped. The work already holds a token of its own (`ctx.cancel`, the
fresh one `waited` gives it); `waited` cancels that token when the wait ends early, and
the tool answers on its own path:

- `wait` — `ended()`: the rows that arrived, or how long nothing came. The same sentence
  its own deadline gives.
- `new`/`check` — `talk` reads the window, because a wait that ends early is a `check`
  that waited for no time at all — SPEC's `Result`, on disk as it stands.
- `<tool>(**args)` (`Forward`) — the command said nothing, so the `Result` has `output:
  ""`, `exit: null`, and the time it spent.

`waited` then takes that answer off the channel, with `EARLY_RETURN` (1s) as a grace. A
tool that never watches its token — a file read — cannot answer early; it is aborted and
`stopped()` says the work stopped before it could say how it went. That is the only
sentence left about a call's ending, and it is about the call, not about the verb.

No tool learns which verb ended the turn. `session.interrupted()` is read by the status
only, and `StopSignal::resumable()` by `interrupt` before it fires, so "idle (cancelled)"
and "idle (interrupted)" are still told apart where a window reads them.

## Measured, one real server per arm

| verb | tool call | `tool_result` | status |
| --- | --- | --- | --- |
| `interrupt_session` + `wait` | `wait 120s` | `nothing arrived within 1.8s; wait again, …` | idle / interrupted |
| `interrupt_session` + `new` | `new bash -c "sleep 60"` | `{"id":"sleeper/bash/1","output":"","time":"1.635s","matched":false,"exit":null}` | idle / interrupted |
| `send_message` + `wait` | `wait 120s` | `1 row(s) arrived while this call waited (a message)…` | idle / done |
| `cancel` + `wait` | `wait 120s` | `nothing arrived within 1.7s; …` | idle / cancelled, `waiting_until` set, one `turn` row kept |

All four wrote `tool_result` with `tool_calls.status=ok`; only the interrupt arm leaves
`canceled=0`, `waiting_until=NULL` and no pending task.

## Two names that did not survive

`early` was a token I added to `ToolContext` for this, then took back out: the work already
has a token, and "this call's wait is over" is what cancelling it means. No new field, no
new signal.

`bash` is not a SPEC tool either — it is a row in `ship/config/config.toml` reached through
`<tool>(**args)`. The SPEC tool that starts a program is `new`, which is what the second
arm above uses.
