# A running call counts from the server's start

Date: 2026-11-19. Worktree `.worktrees/tool-clock`, branch `feature/tool-clock`.

## The ask

> tool run 的过程中要在 UI 上实时显示已跑的时间

Before this, the elapsed time of a tool call existed only as a word in the verdict:
the server measured `run_ms` and spelled it into the closing `Head` (`✓ 1.2s`). While
the call ran, the wire carried nothing about time at all, and the window had no frame
source of its own.

## The decision the user made

**A — the start travels on the wire.** Asked which clock the duration counts from:

- A: `ToolCallEvent.started_at`, the server's own instant, also in the call's stored
  payload so a replay carries it. Every window agrees; a window opened mid-run counts
  from the beginning of the call.
- B: the window stamps its own arrival. No field, and wrong in exactly the two cases
  that matter (mid-run attach, two windows on one session).

The user answered `A`.

## What changed

- `protocol`: `ToolCallEvent.started_at` (epoch millis, `serde(default)`, `0` = no
  start kept). `spent()` moved here from `server/agent.rs` so a running clock and a
  verdict are one shape.
- `server`: one `now_millis()` at the announcement feeds the event, the `tool_call`
  payload and `tool_calls.started_at` (`start_tool_call` now takes it); `run_one_call`
  takes it too. `MessageRow.started_at` reads the payload, so `row_to_event` replays
  the true start.
- `client`: `Sink::tool_call` carries the start; `RenderMessage::ToolCall` carries it
  over the pipe; `CALL_KEY` / `RESULT_KEY` name the `c:<id>` / `t:<id>` pairing once.
- `tui`: `Tui.running: HashMap<key, RunClock>`; `describe` starts a clock, `tick`
  spells `now - started` and writes only changed rows, `stop_clock` ends it on the
  result's last chunk; `Entry::live` + `Entry::call_label` draw it on the call's own
  summary row (never in `tag`, which every reflow re-derives); `pipe.rs` gains a 100ms
  branch disabled unless `Tui::is_running`.

## What was observed

1. Real TUI in a ConPTY (`scripts/tui_live.py`, fake endpoint, `exec` of `sleep 4`):
   `▸ exec·bash 107ms`, `216ms`, ... `4.4s` — one frame per ~107ms while the tool ran;
   then one frame with `▸ exec·bash` and `✓ 4.5s` below it, and no further frames.
2. The live wire (`python .dev/probe_wire.py <home> <port>` against the harness's
   server): `tool_call` event carried `"started_at": 1795085322103`, read 207ms later;
   the closing `tool_answer` head spelled `4.3s`.
3. The stored row (`sqlite3 ... select display from messages where kind='tool_call'`)
   kept `"started_at":1795084999906`, and `messages_between` — the query a replay
   reads — selects that `display` column.

## Open

Nothing in code. The one sentence SPEC needed landed with this branch
(`## UI`, the events block):

```
      |toolcall{tool_name·[*summary_args]·description·time}: while the tool runs,
        `time` is what it has spent so far, redrawn as it grows; it leaves the row
        when `tool_result`'s own `time` states the one the server measured
```

`.dev/clock.script`, `.dev/idle.script` and `.dev/probe_wire.py` are the three
records of the runs above (`.dev` is gitignored).

## A window's server dies with its terminal (2026-11-19)

While this branch was being verified, the user's own window died
(`feature/tool-clock` was untouched by it). What was established:

- The old tree's server logged nothing after 18:53:55.599, so it was not a
  `cancel`: a retire writes `cancel` and a stopping line.
- No Application Error, no WER report, no `4689` (process-exit auditing is off on
  this machine): the process was terminated from outside, not crashed.
- No `WindowsTerminal.exe` older than 18:54:35 survives, so the console that
  hosted the window is gone, and everything under it went together.
- Two `gongga-relay.exe` processes from 11-15 are orphans that hold the relay's
  name -- they made the *new* server at 18:55:12 install a hash-suffixed copy,
  which is not evidence that the old tree's relay outlived it.
