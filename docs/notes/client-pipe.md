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
| `WindowFacts` | `WindowJournal` answers from `CursorMessage`, the frame's `Layout` and the disk `Cache`; stamps and floors survive a restart | `Log::landed`'s `Record::Seal` rewrite (A-4) is not needed for a refill |
| `room_of` | the pipe front-end never calls `note_room`, so it answers 1 | a viewport report from the UI, or a client-side fold (A-4) |
| `known_list` / `repoint` | `Roster` and `WindowJournal` are built from `client_home` + `endpoint`; `Layout` writes the list, `Forget` clears the journal and its files, `repoint` re-points both and sends `Repoint` | — |
| completion vocabulary | `Vocabulary { version }` on a real `paths_version` change; `Complete` intent is answered by `Completion` from the client's own completer | — |
| server ownership | `Server { label, ours }` on every `set_server`, spelled from `Server::label` | — |
| `restore_input` | `Draft { text }` puts the unsent line back in the input | — |

None of these adds a second protocol: they are variants of `RenderMessage` plus a
local journal. All four control-frame gaps are now closed and the TUI speaks the
pipe by default (A-3, 2026-11-09); what remains is the A-4 fold-state split and
the multi-client L3. See `tui-pipe.md`.

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

## Verification after A-2 (2026-11-09)

`client/window_journal.rs` and `client/window_roster.rs` each carry `#[cfg(test)]`
cases that write under a throwaway root, drop the value, then read the same
numbers back in a fresh `WindowJournal` / `Roster`. `repoint` is checked to forget
the old endpoint's stamp. The real-process table above was re-run after A-2 with
the same results; an empty `Layout` writes no roster file, which is
`Roster::know`'s intended no-op. Machine-readable record:
`.dev/verify/<sha>-journal2.json`.

## Verification of the control frames (2026-11-09)

`render_wire.rs` carries `#[cfg(test)]` cases that round-trip all five new frames
and decode a `Complete` intent. A real process under a throwaway home produced
`completion` (twice, one with a cursor inside a UTF-8 character, clamped safely),
`draft` (offline submit), `repoint` and `server` (`/connect_server`) frames, all
`exit=0` with empty stderr. `vocabulary` is covered by the round-trip test: an
offline run never changes `paths_version`. Record:
`.dev/verify/<sha>-frames.json`.

**Isolate `client_home` in every real-process run.** It comes from
`--client-home` / `GONGGA_CLIENT_HOME`, *not* `--home` / `GONGGA_HOME`. One early
run used only `--home` and therefore read the operator's real
`~/.gongga-client/roster`, then wrote the normalised offline statuses back to it.
That is what a TUI start does too, but a verification must never touch it. Pass
both `--home`, `--client-home`, `GONGGA_HOME` and `GONGGA_CLIENT_HOME` pointing
at the throwaway directory.
