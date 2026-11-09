# tui/pipe.rs — the window as a pipe host

A-3 of `DESIGN.md` §1. `gongga-tui` is now a front-end of the `gongga-client`
process: it no longer runs `runtime::run` itself.

## The two ends

`pipe::run` finds `gongga-client` beside its own executable (a bundle extracts
both together), passes the connection flags plus `--parent <ui pid>`, and pipes
stdin, stdout and stderr. stdin carries one `UiIntent` per line, stdout one
`RenderMessage` per line, and stderr goes to `<client-home>/client.log` so a
diagnostic never lands in the alternate screen.

One spawned task owns stdout and pushes decoded frames into an unbounded
channel. The main loop `select!`s over three things: `Tui::next_event()` (the
keyboard), the frame channel, and `closing::asked()` (the console going away).
Closing stdin is the window leaving; an ended stdout is the client leaving.
Neither polls the other's pid.

## Drawing is still `TuiSink`

`run.rs::apply_frame` turns the 12 drawing frames back into `Sink` calls on the
same `TuiSink` the in-process loop used, so a line is drawn exactly once, in the
same order. The 6 control frames are not drawings and land on the window's own
view state:

| frame | the window does |
| --- | --- |
| `Draft` | `restore_input` |
| `Server` | `set_server_text` (the header's own / not own label) |
| `Repoint` | `drop_windows` |
| `Vocabulary` | answer with one `Complete` intent |
| `Completion` | `set_completion` (the client's candidates) |
| `StickySession` | `set_sticky_session` |

`RenderMessage::StickySession` has no `Sink` method in this front-end, so it is
handled here rather than by `apply_frame`'s drawing half.

## What the window no longer keeps

`Tui::for_pipe` uses an empty `Completer` and `Panes::new()` (no `Cache`). A
changed input line sends `UiIntent::Complete`; the client's `SharedCompleter`
answers with `Completion`, so command syntax never crosses to the UI.
`set_layout` skips `Roster::know` in pipe mode: the client owns that file.

`Panes::keep_bodies()` turns off `end_frame`'s eviction. Without a journal to
read cold bodies back from, dropping them would lose rows, so a pipe window
keeps them in memory. That is the one cost of a purely drawing front-end.

`Screen::server_text` lets the header show the label the client spelled instead
of requiring a local `Server` value; `run.rs::Frontend for Tui` still uses
`Screen::server` on the in-process path.

## Deliberate limits

- `GONGGA_TUI_IN_PROCESS=1` still reaches the old same-process `interactive()`.
  It exists to compare the two paths and should be deleted, together with
  `Tui`'s `Frontend` / `WindowFacts` / `WindowView` impls, once shape 2 settles.
- `Entry`'s fold state and `Ledger.pages` remain in the TUI (A-4). The client's
  `WindowJournal` answers a refill from appended records and resends the rows,
  so the UI does not need `rewind` / `older` / `landed`.
- A malformed stdout frame is skipped: the UI has no journal to record it in,
  and writing to the terminal would corrupt the screen.

## Verification

`tui_live.py` window-state and wheel scenarios pass with the default pipe mode
(no `GONGGA_TUI_IN_PROCESS` set), and `<client-home>/client.log` proves the
child was started. `client_state_selftest.sh` still passes. Machine-readable
record: `.dev/verify/<sha>-a3.json`.
