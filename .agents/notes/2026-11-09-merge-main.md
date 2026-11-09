# Merging main into the client split

2026-11-09, `.worktrees/client-split`, branch `feature/client-split`.
Committed as `a3d22f1`, parents `153251d` (split) and `2e7914a` (main).

## What happened

main had advanced 51 commits overnight. The main checkout itself is busy: it
holds uncommitted `SPEC.md`, several `crates/server/src/*` files and a new
script. None of it was touched; every operation here ran in the worktree.

The only conflict was `crates/tui/src/run.rs`: main still carries the 885-line
loop, the split has a 159-line wrapper over `client::runtime::run`. Keeping ours
was right, but **not free**. main's `b19018f` changed `server::stop` to answer
`Result<Option<u32>>` and added a real behaviour: a server that survives cancel
and kill still holds `server.lock`, so starting a replacement would leave the
window with no server at all. That branch lived in the loop main still had, so
it had to be ported into `client::runtime.rs` by hand. Recorded in
`docs/notes/client-runtime.md`.

main also raised `crates/server/ship.toml` to protocol 7 while this worktree's
`ship` checkout was still on protocol 4, so the pre-commit hook's
`ship.sh --check --local` was a genuine drift (exit 1). The main checkout's own
content repository already had `d822e78` (protocol 7), so a local-only fetch
from `C:/resource/gongga/ship` moved just this worktree's checkout. No push, no
change to the outer gitlink (`.gitmodules` says `ignore = all`), and both
`--check --local` and `--check` now answer 0.

Full gates and the two L3 runs are in `.dev/verify/a3d22f1-merge.json`.

## The fork in the road for step 6

Step 6 is "the TUI spawns the client binary and only draws". It cannot be done
as a small step, and three of the four ways forward touch a verdict already
made. This needs the user's call rather than a guess.

| option | what it does | cost |
| --- | --- | --- |
| A. Move the journal first | `log.rs` / `panes.rs` data layer (~3000 lines) moves into client; `WindowFacts` is answered locally, `CursorMessage` stays internal, then the TUI becomes a drawing-only front-end | big mechanical migration, but every verdict holds and step 6 becomes small |
| B. Put cursor frames on the wire temporarily | TUI keeps its journal, `CursorMessage` crosses the pipe, step 6 lands now; journal moves later and the frames come back | breaks "CursorMessage is only consumed inside the client" until the follow-up; two processes own one window's state |
| C. Let the UI report viewport facts | add a `UiIntent` that pushes pane height / drawn windows, so `room_of` and `drawn` can be answered without moving the journal | contradicts "WindowFacts is a local read-only fact, not a cross-process query"; the client would hold a second copy of the UI's state |

Also owed before step 6 is complete, independent of the choice: four control
frames (completion vocabulary, server ownership for the header, `repoint`
cleanup, giving an unsent line back as a draft). Completion is the awkward one:
`SharedCompleter` is constructed by `runtime::run` and handed to the UI factory,
so a front-end in another process cannot use it without either moving
completion client-side or shipping the catalog over the pipe.

## Still open

- `735cc41` still carries a stray WIP hunk in the middle of the split series;
  squashing needs permission (whiting-dev asks for squash-then-merge).
- `scripts/tui_live.py` still needs the cover-dismissal prefix to run its
  shipped scenarios; making that permanent needs permission to edit the harness.
