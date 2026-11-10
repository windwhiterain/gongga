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

`run.rs::apply_frame` turns the 12 drawing frames back into `Sink` calls on
`TuiSink`, so a line is drawn exactly once, in the same order. The 6 control frames are not drawings and land on the window's own
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
The client records the session list itself, in `PipeFrontend::send(Layout)`.

A pipe window keeps every body in memory because there is no journal on this
side to read one back from. After A-4 the eviction path is gone altogether:
`keep_bodies`, `begin_frame`, `end_frame`, `mark_drawn`, `note_room`, `seed` and
`evict` were deleted from `Panes` / `render.rs`, and `Log` no longer owns a
`WindowJournal`. The only local window fact left is `Panes.wants`, a
`BTreeSet<String>` that a wheel past the top fills and `take_wants` empties into
`UiIntent::Older`.

`Screen::server_text` is the header's only source of the own / not own words; the
client spells them, so the TUI needs no local `Server` value. The same-process
loop that once computed them here is gone (2026-11-10), together with `Tui`'s
`Frontend` / `WindowFacts` / `WindowView` impls, `run.rs::interactive`, and the
`TuiSink` overrides for the five cursor methods (`Sink`'s empty defaults suffice).

## Deliberate limits

- The pipe is the only entry point. `GONGGA_TUI_IN_PROCESS`, `run.rs::interactive`
  and the three trait impls it needed were deleted on 2026-11-10; so were the
  `Tui` fields and methods that only that path used, and `crates/tui/src/roster.rs`
  (a one-line re-export after A-2).
- A-4 is done (2026-11-10, verdict 甲). `Entry`'s fold state and a `Ledger`
  without `pages` remain in the TUI; the disk cache, `Record::Seal`, replay,
  seeding and eviction chains are gone. The client's journal only records
  stamp / floor. See `DESIGN.md` §1, "A-4", for the deleted list and the
  before/after frame comparison.
- A malformed stdout frame is skipped: the UI has no journal to record it in,
  and writing to the terminal would corrupt the screen.

## Verification

`tui_live.py` window-state and wheel scenarios pass with the default pipe mode,
and `<client-home>/client.log` proves the child was started.
`client_state_selftest.sh` and `multi_client_selftest.sh` still pass.
After A-4 the same custom draw script was run once on the pre-A-4 build and once
on the slimmed build: the final 30-row terminal grids were identical.
Machine-readable records: `.dev/verify/<sha>-a3.json`,
`.dev/verify/<sha>-multi-client.json`, `.dev/verify/<sha>-a4-step2.json`.
