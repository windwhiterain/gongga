# 2026-11-10 — `interrupt_session` was a `cancel` wearing its name

The operator asked two questions about one behaviour: "I sent a message while it waited
on a tool and no tool result came back", then "I interrupted a session, why does it say
`cancelled`?" The second is the root; the first is the same design seen from the other
side.

## SPEC, quoted

```python
fn interrupt_session(id: Session):
  # end tool waiting, return result as tool call result.
  # stop llm streaming and loop, everything received must be saved to context.
```

Nothing else. No flag, no rerun -- and `cancel`'s own section is where those words live
("set canceled flag to them, restart will auto continue session with cancel flag").

## What the code did

`interrupt_session` called `interrupt(d, id, true)`, and `cancel` called
`interrupt(d, session.name(), true)` for every running session: one function, one set of
arguments, one token. So an interrupt inherited everything cancel says about itself:

| cancel's word | what an interrupt showed |
| --- | --- |
| `mark_resumable()` before the token | the turn was `preserve`d, its `turn` row kept, and the next start resumed a turn nobody asked to resume |
| `Event::Status{detail: "cancelled"}` | the window drew `idle (cancelled)` |
| `WaitTool::cut` on `resumable` | the model was told "the server is stopping: … the next start resumes it" |

The repository's own doc (`docs/notes/server-commands.md`, `docs/notes/server-session.md`)
already said `interrupt_session` never sets the resumable mark. The code was the only
thing that disagreed with it.

## What it does now

`interrupt` takes the verb (`Stop::Cancel | Interrupt | Delete`) instead of a `keep`
bool, because the bool could not say which aftermath was wanted:

- `Cancel` — `mark_resumable`, keep the `turn` row: a restart continues it.
- `Interrupt` — `mark_interrupted`, no resumable mark: the turn ends where it stands and
  `finish_turn` drops its row.
- `Delete` — neither, and the queue is cleared: the session is going away.

The session carries `interrupted` next to `resumable`, cleared by `new_turn` so it
describes the turn in flight and never the next one. A tool call cut by the token reads
it through `StopSignal` and picks `Cut::Interrupted` instead of `Cut::Cancelled`, which
is what its `cut` answer and the session's status detail are written from.

The wire still says `outcome: "cancelled"` -- `TurnOutcome` is a protocol enum with
three variants, and a fourth is a compatibility break for every client older than the
server. The status `detail` is a free string, so the word a window draws is the right
one; adding the variant is its own decision, and it is not made here.

## Measured

One server per arm, a fake model whose first call is `wait` for 120s, and a client
pumping the event stream so the status event is caught as it is published
(`.dev/probe_interrupt.sh`):

| verb | status a window is told | the waiting tool's answer | what the next start owes |
| --- | --- | --- | --- |
| `interrupt_session` | `idle`, detail `interrupted`, outcome `cancelled` | `` `interrupt_session` ended this wait: the turn stops with this answer, and what the session had received is saved to its context `` | `canceled=0`, `waiting_until=NULL`, `pending_tasks` empty |
| `send_message` | `idle`, detail none, outcome `done` | `1 row(s) arrived while this call waited (a message); they are delivered at the next step boundary` | nothing |
| `cancel` | `idle`, detail `cancelled`, outcome `cancelled` | `the server is stopping: this wait ends here, but the turn is kept and the next start resumes it.` | `waiting_until` set, `pending_tasks` = `[turn 1]` |

Every arm wrote its `tool_result` row and `tool_calls.status = ok`: the first question's
"no result" is not reproducible on `main` for `wait`. The one path that really leaves a
call unanswered is a stop that lands while the model is still streaming that call -- the
`tool_call` row does not exist yet, so there is nothing to answer, and `answer_interrupted`
publishes an empty `ToolAnswer` for a call a window may already have drawn.

## Gates

`cargo build`, `rustfmt --check` on the four touched files, `comments-check` on each of
them (all clean). `cargo fmt --check` for the tree fails on `crates/tui/src/vt_input.rs`,
which another session is editing in this checkout right now -- not this change, and not
staged for it.
