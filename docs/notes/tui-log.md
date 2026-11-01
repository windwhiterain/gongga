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

## The mark, the fold, and Entry::sealed

SPEC's `events` is a list of *items*, one block each, and the item that is not the
answer folds: `abstract{symbol, reasoning|toolcall|tool_result}` is one summary row
(two for a reasoning item, which keeps its beginning and its end) until it is clicked.
`output`, `cmd` and `error` are their own shapes and never fold.

So a sealed block of a folding kind is an `abstract`. While it streams it shows the
rows it is writing -- SPEC's "streaming: currently stream, block_size" -- and sealing
is what folds it. That is why `seal` recomputes the row count around the flip: the
fold changes how many rows the block contributes, and a block that folded without
telling the indexes would move everything behind it.

`expanded` is the operator's answer to the fold, and it is the only state a fold has.
`toggle` flips it for one key and shifts both indexes by the difference, which is the
same arithmetic a seal does. A click finds its block through `block_at`, which reads
the rows the frame drew -- so a press on a summary line is a press on the item.

The mark in front of an item (`LineKind::symbol`) is the front-end's, not the wire's:
the server says what a row *is* (`HeadKind`), and this side says what that looks like.
It is drawn with the block's label on the top visible row and measured by `reflow` as
part of it, so a marked row wraps as far as it is actually drawn.

## A part that ends with an empty chunk

`done` with no text and no label is how a streamed part closes, and sealing is what
folds an item into SPEC's `abstract`. It used to be swallowed by the blank-chunk rule --
which exists for a *shell's* exit sentinel, a different thing -- so a call that
streamed stayed open and never folded.

## A reasoning item's two rows

SPEC's `reasoning{beginning (line), ending (line)}`, taken from the block's own wrapped
rows (`row(0)` and the last of `starts`), not from the newlines in the text.

That was the bug this fixed. The first version read `text.lines()`, which is right only
if the model wrote several lines -- and most providers send one summary paragraph with
no newline in it, so the folded view drew one cut row and no end at all. The start and
the end of what was written are a property of the *block*, not of the author's line
breaks: at any width the first row and the last row are exactly those two, and the `…`
between them says the middle was left out rather than that the text ended there.

A provider that does send paragraphs gets the same answer, one paragraph boundary
instead of a wrap.

## Log::relabel

A label that arrives after its block opened, because the outcome of a call is only
known when it ends: `ok · 1.2s ` in front of the output the block already holds.

The label is not text -- that is the whole point of `Entry::tag` -- so setting it is a
reflow, not a prefix: a longer label eats the first row's columns and pushes the
block's own text down. The row count can change, so both indexes are corrected around
it (`contributions` before and after) or every block behind it would be off by the
difference.

A key nobody opened is nothing, and a label already in place is left alone: the caller
says this once per stream, but a repeated write would rebuild the wrap for nothing.

## Entry::tag

The label the block opened with, kept *out* of `text`. Two reasons, and both are
about cutting: a cap hides a block's head rows, and a renderer clips a row at the
window's edge -- either one would take the label with the text if the label were
part of the text. So `text` is what the model or the shell said, and the label is
put back at draw time (see `Entry::view_row`).

It used to be glued onto the first chunk (`format!("{tag}{text}")`), which is how
a block whose label had scrolled off the top ended up unnamed on screen.

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

The row the label is printed on starts `tag.width()` columns in, so the text after
it wraps where it would have wrapped with the label glued in front. The label is
not in the text; the room it takes is in the wrap.

## Entry::row

The row runs from its own start to the next row's start, which is one byte past
the newline that forced the break -- so trimming a trailing newline is exactly
the difference between the two offsets. That is also why a trailing newline
yields a final empty row: it starts at the end of the text and has nothing after
it.

## Entry::view_row

The label goes on the block's own first row, or -- when a cap hid that row -- on the
first row the cap left. A window that cuts a paragraph in the middle is not the cap's
doing, and printing the label there put `[peer]` in front of a row that was never the
start of anything. Either way the label has to be *somewhere* the frame shows, or a
capped block stops saying what it is.

Prepending the label to a wrapped row pushes its tail past the window's edge, and
the renderer clips it. That clip loses nothing: a wrapped row's tail is the start
of the row below it, which is also on screen -- except for the newest row, whose
tail is text that has not got to the window yet anyway.

## Wrap

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

## View and Ledger

One `Log` holds the blocks; a `Ledger` is one way of reading them -- the merged one,
which holds every block in arrival order, or one window's own. A ledger is a list of
block indices plus a row index over them, so "which block holds row N" is answered
per view in O(log blocks) and a frame never walks the log for anybody's benefit.

The same block is counted twice, once per ledger that holds it, and the two counts can
differ: a window may cap its folding blocks apart from the rest, so
`contributions` returns both and every mutation moves both sums.

## Log::headlines (gone)

One row per block, for a view drawn as "one line per event" -- the `line` render mode
SPEC no longer has. `Log::headline` went with the mode: a session is either a window
with its wrapped rows or a name in the list, so nothing asks for a block flattened to
one row.

## Log::set_hidden

`hide_session` is a view: the merged ledger does not hold the hidden window's
blocks, so no frame draws them -- the ones drawn before the command included. The
blocks stay in `entries` and in their own window's ledger, so `show_session` is a
rebuild and not a re-send.

A block whose window is hidden therefore has a position in its own ledger and none
in the merged one (`Entry::merged_at`), and `shift` moves a row sum only where a
sum exists. That is the one place a block is in fewer than two ledgers.

## Log::forget

The only thing here that takes rows away, and it takes them one window at a time: that
name is being handed to a new session. The blocks go, the window's ledger goes, and any
stream still open in it goes too -- a half-written token must not append to the next
session's block because it happens to carry the same key.

The surviving blocks are renumbered, so the open streams of other windows keep naming
their own blocks; `rebuild` then re-derives both ledgers from what is left. Nothing else
removes a block: a cap trims rows from a view, and a hidden window keeps every row it
had.

## Log::set_block_size

`set_event_ui --block_size <lines>`: how many rows one block may fill. A display
preference, so it lives with the front-end and not with the events -- the same
stream arrives either way, only the number of rows it occupies changes.

The rows past the cap are not in the row index at all. That is what makes the
block *auto-scroll* in the SPEC's sense: it stops pushing the blocks below it up,
and it stops pushing the blocks above it off the screen. It also means those rows
cannot be scrolled back to in this window, which is the deliberate half of the
deal: the transcript is the database's job (`get_messages`), the screen is a live
view.

`0` caps nothing, and it is the only way to say so. The setting it replaces had a
mode list, and the list was two words for "fold" and "do not fold".

Changing it rebuilds the index and re-wraps nothing: a cap counts rows, and a
block's rows are measured by width alone. The old `set_detail` had to clear every
open key because the mode changed which rows a chunk joined; a number of rows does
not, so an open block stays open across the change.

## Log::room — the exemption

Only SPEC's folding kinds are capped: reasoning, tool calls and tool results.
The assistant's answer, commands, command results, ordinary messages and errors
keep every row. In particular a session-related `cmd_result` is not tool output,
so `block_size` must not cut it off.

The folding kinds describe progress, which is worth keeping short while it
streams. Their complete text is still retained for an opened item.

## The byte budget that went with the fold

Keeping only the head of a folded block was a limit (`FOLD_KEEP = 4096`) written
into the code, and this repository forbids limits an operator cannot lift. It
existed because the head-fixed row could only ever show the front of the text.
A capped block keeps its whole text like any other block and simply counts fewer
of its rows, so there is nothing to discard and no budget to set.


## Log::resolve — a call and its result are one item

`c:<call_id>` is the identity of both the call and its result. A result does not
open `t:<call_id>` or a per-stream block: it reopens the call, appends its output
and applies the final verdict to the same summary. stdout and stderr never decide
that verdict. `Sink::tool_call` passes the structured head through to the TUI,
so an opened call shows `CALL`, named fields with indented multi-line values,
then `RESULT` and its raw output. Both folded and framed titles read
`verdict and elapsed time · call summary`, keeping time next to the status mark
regardless of how many arguments follow.

Before the verdict, the unexpanded view reads the live result buffer directly:
every output chunk appends visible rows, so a long compilation can be monitored
without opening the item. Opening it switches to the structured sections, where
later chunks continue to appear under `RESULT`. Only the final verdict seals and
folds the item; receiving a structured call head during the run does neither.

## Entry::boxed — one expanded shape

Every opened folding item has a frame, not only an item taller than its cap. The
cap limits body rows; the top and bottom rules always surround that body. Text is
reflowed two columns narrower while expanded, before row offsets and scroll bounds
are calculated. Structured section text starts immediately inside the left
border, without reserving the folded row's symbol padding. Soft-wrapped field and
output rows retain their own indentation. The renderer and index describe the
same rectangle, so a long row cannot push the right rule off the pane. Toggling an item
updates those offsets immediately, so click hit-testing follows the frame too.

With a `block_size` of 1 through 6, the pointer's wheel belongs to the opened
item even at either end. Above 6, or with no cap, exhaustion in the wheel's
direction lets the event continue to the outer pane. While the inner item can
still move, it always keeps the wheel.

## Log::stream — one label, one block

A chunk wearing a label the open block does not carry seals that block and opens a
new one. Two events cannot share a row: glued together they read as a word nobody
typed.

The older rule was the reverse -- a shared block rewrote its one row in place, and
it stayed open across `done` because it belonged to a session rather than to a
part. Both halves are gone: the grouping that made a block belong to a session was
a workaround for having one window for every session, and windows now do that
properly (see `docs/notes/tui-panes.md`).

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
rows of the *shown* rows, full stop. A block grows and its older rows leave the
index -- either above the window, or past its cap and gone from this view -- so the
walk starts at the block holding the first visible row (`RowIndex::line_at`, O(log
blocks)) and a frame still costs what one screen holds.

A capped block contributes `min(rows, cap)` and the window's rows of it are its
last ones: the head is skipped, not trimmed. That skip is per-block, so the window
is not a tail of text but a tail of *blocks* -- which is what makes two sessions
streaming at once readable side by side.

## Log::stream — a blank chunk opens nothing
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

A block taller than the window shows its newest rows and repeats its label on the
first row the window holds. Stated as a test so the label rule stays a decision
rather than a surprise.

## a_capped_block_shows_its_newest_rows

The cap is a height, not a rewrite: the block keeps growing in place, `Log::rows`
counts only what it may show, and the rows past the cap are the *older* ones.

## a_capped_block_follows_its_newest_text

What "auto scroll" means in one row of a wrapped block: the row shown is the last
one, not the first. The folded mode kept the head on purpose -- a row that moved as
text arrived read as scrolling rather than writing -- and the SPEC asked for the
moving. Both cannot be kept, so the row is the tail and the block stops shoving
neighbours away.

## the_rows_a_cap_took_are_not_scrollable

Scrolling back finds the block above, never the capped block's own head. Cheap to
state and the one operators ask about; the answer is that the full text is a
`get_messages` away.

## the_cap_leaves_the_answer_alone

`LineKind::Assistant` keeps every row it grows, at any `block_size`.

## a_new_label_opens_a_new_block

Two parts of a turn, two labels, two blocks -- and the first block took nothing
more after it was sealed, even though the key stayed the same.

## a_capped_block_stays_capped_at_any_width

A cap counts rows of the width being measured, so a resize cannot leave a block
taller than its cap.

## a_blank_chunk_opens_no_block
## a_blank_chunk_opens_no_block

The sentinel's empty line must not become a row. Without this, one blank row per
command, in the error colour.

## a_sealed_block_has_no_trailing_blank_row

A block's trailing newline is trimmed when it seals, while a blank line *inside*
the block survives.

## headline (gone)

The newest block of one view, as one row: that is what a `line` session showed. Not
`window`'s rows -- those are wrapped rows of a block, so a sentence's continuation and
a trailing blank row would both be what the session "was on". The block's tag was left
out, because the row that drew it already started with the session's name.

Gone with the `line` mode and with the flattening it needed (`one_line`).
