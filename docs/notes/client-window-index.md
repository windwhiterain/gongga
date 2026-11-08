# client/window_index.rs

## window_index.rs

The first piece of the window index that can leave the terminal crate.
`RowIndex` is a 1-based Fenwick tree over per-line row counts: `push` appends a
line, `shift` changes one in the middle, `prefix` / `total` / `line_at` answer
which line holds a given row. It has no ratatui, no unicode width and no frame
state, which is why it moves as-is; all five methods became `pub`, the `tree`
field stayed private. The invariants in `tui-log.md` (`## RowIndex`,
`RowIndex::push`, `RowIndex::line_at`) still describe it.

As of 2026-11-08 the only user is `Ledger.index` inside `crates/tui/src/log.rs`
(14 call sites, all in that file). This step changes type ownership only: the
log still builds the index, still folds lines for the width of a terminal pane,
and `Cache` is still instantiated there.

## window_index.rs: Ledger

`Ledger` followed in the same module: one window's blocks in arrival order,
its `RowIndex`, the per-block row cap, the pages it holds, the block a landing
page starts with, and the width its blocks were last laid out at. All six
fields are pure data; the 50 uses are all `Log` methods in `log.rs`, so the
move changes type ownership only.

The fields are `pub` for now. That is a transitional state: while `Log` is
still in the terminal crate it has to reach them directly. Once `Log` moves
too, the fields become private again and the operations become `Ledger`
methods. Registering that here so the `pub` is not mistaken for a design.

## Why this piece first

Verdict 4 puts the journal in the client. The index is data, the folding is
terminal: `Log` (2475 lines) mixes `Entry` / `Ledger` with `Style`, `Line`,
`Span` and `materialize`, and `Panes::room_of` depends on frame height. Moving
the whole journal in one step would mix a data migration with a rendering
rewrite, so the pure structures come out one at a time: `window_cache.rs`
first, then `RowIndex`, then `Ledger`, then `Log` / `Panes`.

No query protocol crosses the pipe for this. `WindowFacts` is answered by the
client from its own journal once the index lands there; a front-end over the
pipe holds no window facts, and `CursorMessage` still only circulates inside
the client. See `docs/notes/client-refill.md`.

## Not done yet

`Log`, `Panes`, `Hit`, `Entry` and the ratatui folding remain in `crates/tui`.
`interactive`, `TuiSink` and the client binary target are still to come; the
interface for lifting the loop is in `client-frontend.md`.
