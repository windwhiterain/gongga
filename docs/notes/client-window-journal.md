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

`Panes` now holds a `WindowJournal` and delegates `begin_frame`, `mark_drawn`,
`note_room`, `room_of`, `drawn`, `take_wants` and `forget`. `PipeFrontend` delegates
its `WindowFacts` and `take_settled` to the same type. Neither owns the fields any
more, so there is one implementation of "a window nobody drew holds no bodies"
and one of "a pane that has not reported yet still asks for one row".

`Panes::forget` now clears the journal's facts for the deleted window too. Without
it a stale `wants` entry could ask the server for a page of a session that no
longer exists.

## What has not moved yet

`stamp_of` and `floor_of` are still answered by `Log` through the disk `Cache`
and the per-window `Ledger` when the front-end is the TUI. `Log::landed` and
`Log::rewind` walk `entries` to build `Record`s, so they are entangled with the
ratatui folding state in `Entry` (`starts`, `col`, `styled`, `source`). Moving
that is A-2: split `Entry` into a data block (client) and a fold view (TUI), then
let `WindowJournal` own the `Cache` and the `Ledger` map directly. Until then the
pipe front-end's stamps and floors come from `CursorMessage` in memory, which is
enough for it to answer locally but is not persisted across a restart.

## Invariants worth keeping

- `Rewind` keeps the stamp: everything up to it was drawn from rows that still
  exist. Only the unaccounted tail goes.
- `Older` does not move the stamp: a page of the past lands above the held rows,
  so the row the window ends at is unchanged. `Landed` names both the new stamp
  and the new floor.
- `room_of` answers 1 when a pane has not reported. `ask_transcript` floors the
  room at 1 anyway, so a fresh window asks for one row rather than nothing.
- Nothing here is a query protocol. A frame *reports* what it drew; the client
  never asks a front-end what it holds. See `DESIGN.md` §1, "形态 2".
