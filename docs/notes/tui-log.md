# tui/log.rs

## log.rs

The scrollback, and the arithmetic that turns a scroll offset into rows.

Why this is a structure rather than a `Vec<Line>`: a conversation grows without
bound, and everything the renderer does happens once per frame -- which, while a
response streams, is many times a second. Wrapping the whole log on every frame
is what made a long conversation unusable: the cost of one keystroke grew with
the number of messages already on screen, because each frame re-wrapped every
line of the history and allocated a string per *character* of it (the old `wrap`
went through `char::to_string` to measure a char). Five thousand lines meant
millions of allocations per frame.

So nothing on the path a frame takes may walk the log. Three caches make that
true:

* A line caches the byte offset at which each of its wrapped rows starts
  (`Entry::starts`). Drawing a row is then a slice, not a re-wrap, so a frame
  costs what is *visible* and nothing else.
* Appending to a line -- what streaming does, over and over -- resumes that state
  machine from the last row instead of restarting it, so a delta costs what the
  delta is worth, not what the line is worth. Without this, a long streamed
  answer would be re-wrapped from the top on every token: quadratic in the
  answer's own length.
* A Fenwick tree over the per-line row counts answers "which line holds row N" in
  O(log lines) instead of walking. Scrolling to the top of a long log therefore
  costs the same as scrolling one notch at the bottom.

The one operation that has to touch everything is a width change, because every
cached offset was measured for the old width. That happens when the window
changes shape and nowhere else.

## Entry

One line of the log, plus what it takes to draw a row of it without wrapping the
whole line again.

## Entry::starts

Byte offset of the first character of each wrapped row. The length of this list
*is* the row count, which is what the row index sums over.

## Entry::open

Display columns used by the last row: where an append resumes. Appending can only
ever extend the last row or open new ones, so this one number plus the last start
is the whole wrap state.

## Entry::new

Width 0 means "not measured yet" -- the first draw reflows everything at the
width it actually has.

## Entry::reflow

A line always has at least one row, even when it is empty: that is what makes an
empty line a blank row rather than nothing at all.

## Entry::row

The row runs from its own start to the next row's start, which is one byte past
the newline that forced the break -- so trimming a trailing newline is exactly
the difference between the two offsets. That is also why a trailing newline
yields a final empty row: it starts at the end of the text and has nothing after
it.

## Wrap

The greedy wrap state machine, movable so an append can pick it up mid-line.

Greedy wrapping only depends on the current row's column count, which is why this
is one number and why streaming a long line stays linear rather than quadratic.

## Wrap::feed

Feeds `text` through the machine, recording the start of every row it opens in
`starts`. `base` is where `text` begins inside the line, so the offsets land in
line coordinates either way -- a reflow passes 0, an append passes the current
length.

A control character has no width but still advances the row by one, or a row of
invisible bytes would never break.

## RowIndex

Prefix sums over the per-line row counts.

A plain `Vec` of running totals would work for appends and break on streams: a
streamed delta changes a line in the *middle* of the log whenever a tool output
and a response interleave, and every total after it would be wrong. A tree pays
O(log lines) for both the update and the lookup instead.

`tree` is a 1-based Fenwick tree.

## RowIndex::push

The new node has to carry its whole range, not just its own count: a Fenwick node
covers `[i - lowbit(i) + 1, i]`, and the lines it covers were added before it
existed. Filling that in from the prefix sums is what keeps "which line holds row
N" honest after an append.

## RowIndex::line_at

The line holding 0-based row `row`: the first line whose running total ends past
it. Returns the line count when `row` is past the end, so the caller bounds it.

The largest power of two that fits is the classic binary lifting start.

## Viewport

What one frame needs: the rows that fit, borrowed from the log, and how far the
log can be scrolled.

`max_scroll` is the rows hidden above the window when it is pinned to the newest
line. The key handler clamps against it, and only the renderer can work it out.

## Log::width

The width the cached row offsets were measured at. 0 until a frame has been
drawn, because until then nobody knows how wide the terminal is.

## Log::append

Appends to line `line`, which is what a streaming response and a streaming tool
output do for every chunk.

A missing line is ignored rather than panicked on: a stream key outlives its line
only if the caller cleared the log underneath it, and losing a chunk is a better
failure than taking the window down.

## Log::window

The rows for one frame: everything visible, nothing more.

`scroll` counts rows up from the newest line, already what the key handler meant;
it is clamped here because lines can be added or the window can grow between the
key and the frame.

The row index of a line's first row is carried forward instead of asked for
again, so the walk is one pass over the window.

## Log::reflow

Recomputes every row offset when the width changed. The only whole-log walk left:
a resize is a shape change, not something a frame does.

## appending_equals_rewrapping_from_the_top

The invariant the streaming path depends on: appending chunk by chunk must land on
exactly the rows a single reflow of the whole line would.

This is the bug that would otherwise come back as text jumping around mid-answer,
and it is invisible in a screenshot because it only shows up on lines that wrap.

## embedded_newlines_force_breaks

A trailing newline is an empty row, not a dropped one: that is how a blank line
between two paragraphs survives the round trip.

## scrolling_moves_one_row_at_a_time

A window can start in the middle of a line, which is the case a naive "count
lines, not rows" renderer gets wrong.

## scroll_clamps_at_both_ends

A page up from the bottom must show the rows just above the window, and scrolling
past the top must stop at the first row -- not wrap around.

## a_frame_touches_only_a_screenful

A frame must not depend on how long the log is: this is the assertion that
catches a return to wrapping everything per frame.

## a_width_change_reflows_every_line

Back to the wide shape: the old offsets must not survive.

## the_row_index_agrees_with_a_walk

The row index's binary search has to agree with a plain walk, or scrolling lands
on the wrong line while everything else looks fine.
