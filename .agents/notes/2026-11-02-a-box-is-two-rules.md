# A box is two rules, and a value starts at the left edge

From the operator, over a screenshot of an opened `bash` call: the indentation after
the expansion wasted space, and some bash calls in the same window looked pretty while
others did not. Two separate findings, one of them a change and one of them history.

## Why some calls had no title

Not a rendering path. The title argument was renamed under the blocks already on
screen: `98b303b` made SPEC's `description` a `Head` field and dropped bash's `task`
from the summarised set. A call written by the older build (or by a model that still
writes `task`) keeps `task` as an ordinary field: its folded row used to be
`▸ bash, task=…` and is now a bare `▸ bash`, with the sentence only visible on open,
under a name that no longer exists. A seal in the journal is not rewritten by a new
build, so both spellings sit in one window. Left as history: a transcript says what
its model sent, and nothing here migrates it.

## What changed

- `Entry::section_text` writes `name: value` and puts the value's own lines after it at
  the left edge. It used to indent a multi-line value two columns under a `name:` line
  of its own and four columns for every line of it: on a long `command` that was seven
  columns of every row, and it is why a wrapped command line read as indented into
  nothing.
- `log::Edges` is gone; a row is `Rule::{Body, Top, Bottom}`. An opened item's box is a
  rule above and a rule below and has no sides, so a body row is as wide as the window
  -- the sides had cost every row two columns.
- `Entry::content_width` is gone with them: a box no longer reflows two columns
  narrower than the window it is drawn in.
- `reflow` cuts a call's label two columns short while the block is open, because the
  top rule is one column of air and one of dash, and a top rule with no dash on it is a
  box that reads like a folded row.
- `render::framed_row` is `ruled_row`, and `pane_slots`/`draw_panes` draw the sticky
  window's frame with `Borders::TOP | Borders::BOTTOM`: the same finding in the split —
  a border on both sides of a column is two columns of every row in it.

The operator chose both (A: no indent; B: no sides) rather than a hanging indent under
the value, which would have cost more, not less.

## How it was checked

There is no test for it and none was written. A throwaway example built a `Panes` with
one described call and one result, expanded both, and drew a frame into
`ratatui::backend::TestBackend`, printing the buffer as ASCII at widths 40/46/100 and at
the label lengths that make the top rule's fill 0, 1 and many columns; plus a scrolled
case. `render` was made `pub mod` for the length of that run and is private again.
Deleted afterwards, so the numbers in this note are the record.

## Where it stands

The tree was left **uncommitted**: this checkout is in the middle of another session's
merge (`MERGE_HEAD`, `crates/server/src/config.rs` unmerged, `feature/remotes-file`), so
a commit here would either be refused as a partial commit or sweep that merge in. The
same diff is saved at `.dev/tui-box-rules.patch` (`git apply` restores it if the merge is
aborted or resolved from the index), and `target/debug/gongga-tui.exe` is rebuilt, so a
window restarted now draws the new shape.

# The wheel stops where it lands, and a box has a margin

Same day, the operator's next two instructions: delete the overflow (an opened
block's inner scroll handing the wheel to the pane once it ran out), and give an
opened block a configurable margin on each side whose wheel is the pane's.

## Why the handover went

It made one cell mean two things at two moments: a notch over a box moved the box
until the box was at its end, then moved the whole pane. Opening an item is exactly
what makes a pane long enough to scroll, so the switch fired whenever the reader was
least expecting it. `Screen::scroll_at` no longer falls back: the cell decides, once.

What replaces it is geometry. A box gives the window `box_edge` columns on each side
and those columns -- like the box's two rules -- belong to the pane. `0` is a box drawn
edge to edge.

## How it is wired

- `Log::set_box_edge` / `Log::box_edge`; the number lives in `Log` only.
  `Log::inner_width` is the single place a box's width is computed
  (`box_margin` clamps it, so a silly margin cannot make a box with no rows), and
  `render` calls the same `box_margin`, so a rule cannot fall outside its own rows.
- `Ledger::edge` remembers the margin a window was laid out with, so `lay_out` treats
  a new margin exactly like a new width: reflow the hot blocks, read the cold ones
  back, re-derive the index. Setting it costs one layout per window, once.
- `log::Rule` is `Free | Inside | Top | Bottom`: `Body` could not say whether a row is
  a box's, and the renderer has to inset only the box's rows.
- The hit region gained a column: `PaneCell::column`, and `Log::hit` answers nothing
  for a cell in the margin, so both the wheel *and* a click there are the pane's.
- The setting: `set_render --box_edge <columns>` (initial `0`), through the same
  read-modify-write as `--block_size`, announced on every `refresh` via
  `Sink::box_edge`, applied by `Tui::set_box_edge`. `BOX_EDGES` is the Tab list.

Initial `0` on purpose: nothing changes for anyone who does not ask, and the operator
who wants a way to reach the pane under a tall box asks for it. If it should be on by
default, one constant changes.

## How it was checked

- Headless frames at widths 46/52/56, `box_edge` 0 and 2, and the same window with the
  margin raised mid-life: the box re-wraps and the drawn rows match the index.
- `Screen::scroll_at` probed column by column across a pane whose box is at its travel
  end: `x=6,7` (the margin) scroll the pane, `x=8..48` (the box) do not, `x=49..51`
  (the right margin and the spare column) do.
- End to end against a throwaway server on a kernel-assigned port with its own home
  (killed by the lock's pid, after checking the supervisor's `CommandLine`):
  `/set_render --box_edge 2` accepted and saved, `--block_size 3 --box_edge 1` saved
  both, and `--box_edge nonsense` refused with the list of widths.
- `cargo test --workspace` green, `test-layout-check` 0, no new comment-budget
  violation in any file I touched, no new clippy warning in `gongga-tui`.

## The SPEC amendment (user approved, 2026-11-02)

Three lines, in SPEC's own density: the `abstract` row says `box_edge` and `scroll 不溢出到
外层` where it used to say `scroll if block_size!=0`; `- scroll` gains the invariant (an
opened block never hands its scroll out, and its rules and margin are the events'); and
`set_render` gains `[--box_edge <n>](initial: 0)` with the one-line meaning.

## What it still needs

`crates/protocol` carries the client-command table, and the **running** server
announces it in its greeting: the window validates `/set_render --box_edge` against
what that server said, so an old server answers `set_render has no argument
--box_edge`. Hence the evolve after this commit.
