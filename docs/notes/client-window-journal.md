**2026-11-10, the page size**: `room_of` had no writer after A-4 deleted the
frame hooks that fed it, so every page of the past was one row. The drawing now
reports each pane's height as `UiIntent::Room`, and `WindowFacts::note_room` is
the only writer. A pane that has not reported yet still asks for one row.

**A-4 note (2026-11-10)**: the A-1 / A-2 tables below describe the step where
`Panes` and `Log` still held a `WindowJournal`. That copy has since been removed;
see the last section for the current shape.

# client/window_journal.rs

## Why this file exists

`WindowFacts` has always been answered by whichever front-end happened to hold the
window. The TUI answered from `Panes` and `Log`; the pipe front-end answered from
a private `Facts` struct built out of the frames it had just emitted. That is two
copies of one decision, and it is the reason step 6 (the TUI spawns the client)
could not be small: the client would have had nothing local to answer with.

`WindowJournal` is the client's own copy. It carries no terminal type, no ratatui
and no widget state. A frame reports its facts, the cursor half of `Sink` is eaten
here, and `declare` / `answer_*` read the answer through `WindowFacts`.

## What moved in step A-1

| fact | before | now |
| --- | --- | --- |
| `drawn` | `Vec<String>` in `Panes`; the pipe front-end copied `Layout.shown` | `WindowJournal::drawn` |
| `room` | `HashMap<String, usize>` in `Panes`; the pipe front-end returned 0 | `WindowJournal::room` |
| `wants` | `BTreeSet<String>` in `Panes` | `WindowJournal::wants` |
| `stamps` / `floors` / `settled` | a private `Facts` plus a `Vec` in `crates/client/src/main.rs` | `WindowJournal`, fed by `CursorMessage` |

`Panes` now reaches the one `WindowJournal` that `Log` holds and delegates
`begin_frame`, `mark_drawn`, `note_room`, `room_of`, `drawn`, `take_wants` and
`forget`. `PipeFrontend` delegates its `WindowFacts` and `take_settled` to the
same type. Neither owns the fields any more, and after A-2 the two front-ends do
not even own two copies of the journal: `Log` holds it, `Panes` borrows it.

`Panes::forget` now clears the journal's facts for the deleted window too. Without
it a stale `wants` entry could ask the server for a page of a session that no
longer exists.

## What moved in step A-2

| thing | before | now |
| --- | --- | --- |
| disk `Cache` | `Log.cache: Cache`, instantiated in `log.rs` | `WindowJournal.cache`, built by `with_cache(root, endpoint)` |
| `Log::stamp` / `rewind` | wrote the `Cache` directly | delegate to `WindowJournal::stamp` / `rewind` |
| `stamp_of` / `floor_of` (pipe) | in-memory maps from `CursorMessage`, lost on restart | disk `Record::Stamp` / `Floor` / `Rewind`; `stamp_of` reads `stamp_at` |
| session list | `tui/src/roster.rs` | `client/src/window_roster.rs`; `known_list` and `repoint` work on the pipe |
| one journal per front-end | `Panes` held one, `Log` held a `Cache` | `Log` holds the only `WindowJournal`, `Panes` forwards to it |

`consume` now writes the cursor half down: `Transcript` becomes `Stamp`, `Landed`
becomes `Floor` + `Stamp`, `Rewind` becomes `Rewind`. The pipe front-end has no
`Entry` blocks to rewrite, so it appends the two records a refill reads back
instead of calling `Cache::rewrite`. A fresh process answers `stamp_of` from
`Cache::stamp_at` and `floor_of` from the newest `Floor`; two `#[cfg(test)]` cases
cover that a stamp outlives the journal that wrote it and that `repoint` forgets
the old endpoint's facts.

`PipeFrontend::send` writes `Roster` on every `Layout` frame and clears the
journal and its files on `Forget`; `repoint` re-points the journal and the roster
together, because another endpoint is another set of files.

## A-4: the second copy is gone

A-4 (2026-11-10, verdict 甲) deleted the TUI's `Log::landed`,
`Entry::seal_record`, `materialize`, `Record::Seal` writes, `Ledger.pages` and
`page_first`, and every `Panes` method that forwarded to the second
`WindowJournal`. The client's `WindowJournal` is now the only one. The TUI
keeps a `wants` set purely to turn a wheel gesture into `UiIntent::Older`;
stamps, floors, drawn, room and settled are answered here and only here. See
`DESIGN.md` §1, "A-4".

## Invariants worth keeping

- Disk stamps describe fetched transcript ranges, not the live rendered watermark.
  `Landed` merges touching cached intervals; disconnected ranges are not claimed.
- `Rewind` is a rendering frame that drops unfinished blocks, not cached rows.
- `Older` prepends a page without moving the live rendered watermark.
- `Forget` deletes the invalid journal but preserves viewport membership and height;
  only a subsequent layout changes those facts.
- `room_of` answers 1 when a pane has not reported. `ask_transcript` floors the
  room at 1 anyway, so a fresh window asks for one row rather than nothing.
- Nothing here is a query protocol. A frame *reports* what it drew; the client
  never asks a front-end what it holds. See `DESIGN.md` §1, "形态 2".
