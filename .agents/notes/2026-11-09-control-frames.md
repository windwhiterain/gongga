# A-3 preconditions: the four control frames (2026-11-09)

All work in `.worktrees/client-split`; the main checkout was never touched.

## What was owed

`docs/notes/client-pipe.md` listed four gaps that kept the pipe front-end a
skeleton: completion vocabulary, server ownership, repoint cleanup and draft
restore. They are closed. Nothing here is a second protocol: five
`RenderMessage` variants and one `UiIntent` variant.

## Decisions

- **Completion stays in the client.** The tempting shape was a frame carrying
  the catalog, but that would put command syntax in a UI in another language.
  Instead `UiIntent::Complete { line, cursor }` goes to `runtime::run`, which
  clamps the byte cursor to a char boundary, asks its own `SharedCompleter`,
  and hands a `Completion` to `Frontend::show_completion`. The pipe front-end
  encodes it; the TUI keeps computing its popup locally and ignores the
  callback. A first attempt had `PipeFrontend` handle `Complete` in
  `next_event` and return a new `FrontendEvent::Handled`; that forced a
  meaningless empty arm into the runtime match, so it was replaced with the
  trait method. Both front-ends now share one path.
- **`Vocabulary` carries the real `paths_version`.** `WindowFacts::refresh_completion`
  now takes the version, so a UI can dedupe. A local counter would have been a
  fake version. `WindowView::refresh_completion` is unchanged: a popup is the
  UI's own, and only `answer_completion`'s real version change emits a frame.
- **`Server` spells the label.** `label` comes from `Server::label()` and `ours`
  from `is_ours()`, so a UI draws the header without learning the enum.
- **`Forget` / `Draft` / `Repoint`** map one-to-one onto `PipeFrontend` methods
  that were empty or emitted a `Note`.

## Files

- `client/render_wire.rs`: `Draft`, `Server`, `Repoint`, `Vocabulary`,
  `Completion`; `UiIntent::Complete`; two round-trip tests.
- `client/complete.rs`: serde derives on `Candidate`.
- `client/window_refill.rs`: `WindowFacts::refresh_completion(version)`.
- `client/runtime.rs`: `Frontend::show_completion`, the `Complete` arm.
- `client/main.rs`: `PipeFrontend` emits the frames, no local counter.
- `tui/lib.rs`, `tui/run.rs`: signature update and a no-op callback. TUI
  behaviour is unchanged.

## Environment trap (important)

`client_home_of` reads `--client-home` / `GONGGA_CLIENT_HOME`, **not** `--home` /
`GONGGA_HOME`. The first real-process run of this change used only `--home`, so
`Roster` read the operator's real `~/.gongga-client/roster/<endpoint>.toml`
(30 sessions) and `Roster::know` wrote the normalised offline statuses back to
it. That is the same write a TUI start makes, and no data was deleted, but the
file's status values were normalised to `idle` with no backup. It was not
restored. Every later run passed both `--home` and `--client-home` plus both
env vars pointing at `.dev/pipe-frames`. Recorded in `client-pipe.md`.

## Verification

All gates green. `cargo test -p gongga-client --lib` is now 5 tests. Real
process with an isolated home: `completion` (including a cursor landing inside
a UTF-8 character, clamped to the boundary), `draft`, `repoint` and `server`
frames, `exit=0`, empty stderr, every stdout line valid JSON. `vocabulary` is
covered by the round-trip test only; an offline run never changes
`paths_version`. L3 state selftest and TUI live window-state / wheel re-run.

Machine-readable record: `.dev/verify/<sha>-frames.json`.

## Still owed

A-3 (TUI spawns the binary and becomes protocol-only), A-4 (`Entry` fold state
and `Ledger.pages`), ⑦ bundle payload whitelist, ⑧ multi-client L3. The
`tui_live.py` fake-provider issue is unchanged and still blocks any scenario
that sends a model message.
