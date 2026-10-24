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

One *block* of the log, plus what it takes to draw a row of it without wrapping
the whole block again. A block is the unit the window allocates in: a streaming
response or tool output is one block, and it keeps growing in place until it is
sealed.

## Entry::starts

Byte offset of the first character of each wrapped row. The length of this list
*is* the row count, which is what the row index sums over.

## Entry::col

Display columns used by the last row: where an append resumes. Appending can only
ever extend the last row or open new ones, so this one number plus the last start
is the whole wrap state.

## Entry::sealed

No more text can arrive. `Log::push` seals on creation; `Log::stream` seals on
`done`. Sealing is what allows the blank tail to be trimmed; it does not change
how the window allocates rows, because it does not allocate by block at all.

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

## Log::stream

What a streaming response and a streaming tool output call for every chunk. The
key is the block's identity: the first chunk opens a block and every later chunk
appends to that same block, which is why two blocks streaming at once both keep
growing instead of interleaving into one another's rows.

`done` seals the block and forgets the key, so a later chunk under the same key
opens a new block rather than appending to the closed one. A whitespace-only
chunk never opens a block (see below); an empty one with `done` set only clears
the key.

## Log::append

Appends to an entry by position, which `Log::stream` does once it has resolved a
key. A missing line is ignored rather than panicked on: losing a chunk is a
better failure than taking the window down.

## Log::set_detail

A display preference, so it lives with the front-end and not with the events:
the same stream arrives, only the number of rows it occupies changes. Blocks
already on screen keep the shape they were drawn in -- retro-fitting them would
mean re-reading history nobody asked to see again -- and `open` is cleared so the
next chunk of a key opens a block in the new mode instead of appending to one
drawn the old way.

## Log::set_detail — why `folds` lives here

Which *rows a stream joins* is a question about meaning, so the command layer
answers it: in `one` mode it hands a session's parts one key. How many rows one
block occupies is a question about drawing, so it is answered here. Neither layer
has to know the other's half of the rule, and a front-end that renders events
some other way can ignore `Sink::detail` entirely (its default is a no-op).

Content is never folded: `Assistant`, `ToolOutput` and `ToolError` are what the
operator came for. Everything else is progress.

## Entry::reflow_folded

One row, always. The tag is kept out of `text` for exactly this reason: the cut
has to come from the front, and clipping a composed string would take the label
with it.

## Entry::append_folded

A break inside a folded block becomes a space, because the row it lands on is
the only row there is. The buffer keeps its last `FOLD_KEEP` bytes: a row shows
the tail, so the front is the part that can go, and a thinking block that runs
for an hour cannot grow the log without bound.

## Log::stream — a folded block in `one` mode

`done` seals a block, which is right for a key that names one part and wrong for
a key that names a session: the next part arrives with `done` of its own and must
reuse the same row. So a folded block in `one` mode stays open, and a changed tag
restarts it -- one row cannot show two events' text, and gluing them reads as a
word that was never typed.

## Log::window

The rows for one frame: everything visible, nothing more.

`scroll` counts rows up from the newest line, already what the key handler meant;
it is clamped here because lines can be added or the window can grow between the
key and the frame.

The walk starts at the entry holding the window's bottom row (`RowIndex::
line_at`, O(log lines)) and goes upwards, so a scrolled window does not touch the
history below it and a frame still costs what one screen holds.

## Log::window — the window is the tail of the log

No block is reserved space and none is cut out: the window is the last `height`
rows, full stop. A block is one line however long it grows, so a growing block
simply pushes older rows above the window, and they come back when the operator
scrolls. The walk starts at the block holding the first visible row
(`RowIndex::line_at`, O(log blocks)), so a frame still costs what one screen
holds.

The cost of that simplicity: a block taller than the window shows its newest rows
without the session label, because the label is on the block's first row and that
row is above the window. Keeping a block short -- which agent output mostly does
-- is what keeps it named.

## Log::stream — a blank chunk opens nothing

The shell writes one empty line before every exit sentinel, on both streams. The
drain loop reads it as a content line, so every single command used to hand the
front-end a chunk whose text is just `\n`. A chunk like that must not open a
block, or every command leaves a blank row -- and on stderr that row is drawn in
the error colour, so a busy session fills the screen with red blanks.

A whitespace-only chunk is therefore dropped when it would *open* a block. Inside
a block it is still appended, because a blank line between two paragraphs is real
output.

## Log::stream — a sealed block has no blank tail

`done` trims the blank tail off the block's text before sealing it. Without that,
the sentinel's newline leaves one gap after every command. Trimming the text
rather than the rows keeps it correct at a width nobody measured yet: `reflow`
derives the rows from the trimmed text afterwards.

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

## two_open_blocks_grow_together

Two blocks streaming at once each keep growing where they opened, so the second
one never jumps above the first. This is the difference from a per-line model: a
line model has nowhere to put a second stream's growth but after it.

## a_growing_block_scrolls_the_window

A block growing past the window pushes older rows *above* it, not out of the log:
scrolling back finds them.

## a_long_block_shows_its_tail

The tail of a block taller than the window, with no label repeated. This is the
simple model's cost, stated as a test so it stays a decision rather than a
surprise.

## a_blank_chunk_opens_no_block

The sentinel's empty line must not become a row. Without this, one blank row per
command, in the error colour.

## a_sealed_block_has_no_trailing_blank_row

A block's trailing newline is trimmed when it seals, while a blank line *inside*
the block survives.
