# client/main.rs — the pipe front-end

## What this file is

`gongga-client` is the child process a UI starts. It has one stdin pipe carrying
`UiIntent` JSON Lines, one stdout pipe carrying `RenderMessage` JSON Lines, and
the SPEC reliable-UDP link to a server. It links its own library's
`runtime::run`; the loop, the reconnect ladder and the command layer are not
reimplemented here.

This is step ⑤ of `DESIGN.md` §1. Steps ⑥ (TUI spawns this binary) and ⑦ (bundle
payload) are not done, so this program is not on the operator's path yet.

## The shape

```text
UI (parent)                gongga-client (child)                  server
  |  {"intent":"submit",...}   |                                     |
  |--------------------------->|                                     |
  |                           |  PipeFrontend::next_event            |
  |                           |    -> FrontendEvent::Intent          |
  |                           |  runtime::run                        |
  |                           |    -> Sink methods -> PipeSink       |
  |                           |    -> WireSink -> RenderMessage      |
  |                           |    -> CursorMessage (eaten here)     |
  |                           |  writer task -> stdout               |  UDP
  |  {"type":"line",...}      |------------------------------------->|
  |<--------------------------|                                     |
```

`PipeSink` exists because `Frontend::sink()` returns `Box<dyn Sink + '_>` and the
loop calls it many times per iteration. It owns a `WireSink` for exactly one call
and drains it in `Drop`: render frames go to the writer task, cursor frames are
consumed by `client::window_journal::WindowJournal`. Nothing in the library knows
the pipe exists. See `client-window-journal.md`.

## Decisions worth keeping

- **Unbounded channel, one writer task.** `Sink` is synchronous `&mut`, so it can
  never await a write. A bounded `try_send` would have to drop a frame when full
  or block the loop; both break "do not silently lose a frame". The writer task
  only does `write_all` + `flush`. The UI must keep draining stdout (hard point 3).
- **`--parent` is the UI's pid, hidden in `Args`.** A UI passes it in argv; the
  binary hands it to `runtime::run` as `owner_pid`, and `run` passes it to
  `spawn()` instead of `std::process::id()`. `whose_server` compares the greeting's
  owner against the same pid, so a handover is still recognised. When the UI does
  not pass it (running the binary by hand), it falls back to this process's id.
- **stdin EOF is the parent dying.** The OS closes the parent's pipe handles, the
  child's `lines()` returns `None`, and `FrontendEvent::Closed` ends the loop.
  `closing()` is `pending()`; `Quit` is the operator's own way out. No pid polling,
  no second signal.
- **Bad input is said, not dropped.** A line that is not a `UiIntent` produces a
  `Note{kind: Error}` frame and reading continues. Empty lines are skipped.
- **No TTY probing.** This binary *is* protocol mode; a command in argv is
  rejected rather than silently ignored. `gongga-cli` keeps the one-shot mode.
- **stderr.** Only `send()` writes there, on a JSON encode failure. `start()` gives
  the spawned server its own log file, so the protocol stream stays clean. A UI
  should redirect this child's stderr to a file, not pipe it and never read it.
- **`serde_json` escapes newlines inside strings**, so a `Submit` holding whole
  paragraphs and a `Note` echoing it back are each exactly one frame.

## What is still owed (the skeleton's edges)

| gap | today | where it goes |
| --- | --- | --- |
| `WindowFacts` | answered locally by `WindowJournal` from `CursorMessage` and the frame's `Layout`; `known_list` is still empty | stamps and floors persisted once `Log::landed` / `rewind` move (A-2) |
| `room_of` | the pipe front-end never calls `note_room`, so it answers 1 | a viewport report from the UI, or a client-side fold (A-2) |
| completion vocabulary | `refresh_completion` is a no-op | a control frame carrying the catalog, or completion moves client-side |
| server ownership | kept in `server` only for `server_is_ours` | a frame for the header's own / not own label |
| `repoint` | no-op | a frame telling the UI to clear the old endpoint's windows |
| `restore_input` | emits a `Note` with the unsent line | a draft frame that puts the text back in the input |

None of these adds a second protocol: they are variants of `RenderMessage` plus a
local journal. They are the preconditions for step ⑥, where the TUI stops holding
state of its own.

## Verification (real process, 2026-11-08)

Built with `CARGO_TARGET_DIR` local to `.worktrees/client-split`. Every run used a
throwaway home under `.dev/pipe-smoke`, `--client_only`, an explicit `--parent`,
and separate stdout/stderr captures. No server was started.

| input | exit | stdout | stderr |
| --- | --- | --- | --- |
| `{"intent":"quit"}` | 0 | 4 frames (`layout`, `status` x2) | empty |
| empty stdin (EOF) | 0 | 4 frames | empty |
| submit, bad line, older, quit | 0 | 9 frames, incl. `unreadable intent` note | empty |
| `{"intent":"submit","line":"/get_messages\nsecond line"}` , quit | 0 | 9 frames; the echoed line is one frame | empty |

Every stdout line parsed as JSON in Python; the frame with `\n` in its text stayed
a single line. Machine-readable record: `.dev/verify/<sha>-pipe.json`.
