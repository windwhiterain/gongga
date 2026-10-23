# tui/render.rs

## CHROME_ROWS

Rows the layout spends on everything that is not the log or the input: header,
footer and help. The input's own height varies with its text, so it is asked for
separately (see `input_rows`).

## PREFIX

The prompt on the first row, and the padding continuation rows line up behind.

## MAX_INPUT_ROWS

The most rows the input box grows to before it scrolls.

A message is prose, not a file: past a screenful of it, what the operator needs
to see is the answer they are replying to, so the box stops growing.

## draw

The log borrows the screen mutably: it is where the scroll offset gets clamped
against a height that only the renderer can compute.

## input_rows

How many rows the input box needs for `text` at this width, and how many the
window can spare.

Wrapping is part of the answer: the box is one row until the text outgrows the
line, then it is one row per wrapped row -- a long message must stay visible, not
scroll sideways off the edge where nothing can be edited.

The log keeps at least three rows: a box that eats the window would hide the very
thing the message is about.

## content_width

Columns available for text: the prefix on the left, and one spare on the right so
a full row is not wrapped a second time by the terminal itself.

## layout_input

Breaks the input into rows the way the terminal will show it: a `'\n'` is a hard
break, and a row that reaches `width` columns continues on the next one.

The last row always exists, so an empty input -- or one that ends with a newline
-- has somewhere for the cursor to sit.

## draw_log

Draws the log window.

The log hands back exactly the rows that fit, already wrapped and borrowed from
its own buffers, so this walks a screenful and nothing else. The scroll bound
comes back with them because only here is the window height known, and the key
handler needs it between frames.

One column short: a row that exactly fills the width leaves the terminal free to
wrap it again on its own, which double-spaces every full line.

Clamping here as well as in the key handler covers the cases the handler cannot
see: a window that just grew, or a line set that just shrank.

Borrowed, not cloned: the text lives in the log and the spans can point at it.
Cloning every visible row was part of what made a long log slow.

## draw_input

The box stops at its cap rather than growing forever, so a long message scrolls
inside it: the window follows the cursor instead of the cursor leaving the window.

The placeholder explains the empty box; it is not text, so it is only drawn where
the text would have started.

A row that exactly fills the width puts the cursor one past its last column, which
is outside the area: keep it on the edge.

## draw_completion

The window follows the selection. `move_selection` walks the whole list, so drawing
only the first screenful leaves the ninth candidate selected with nothing
highlighted -- and Tab still inserts it.

## the_log_window_shows_the_newest_rows_above_the_footer

Whole-path check without a terminal: layout, window, and the rows the bottom of the
log actually shows. The TUI cannot be run in a test, but "the newest line is the
one above the footer" can be.

10 rows tall: header, six rows of log, footer, input, help.

## scrolling_moves_the_window_by_rows

One row up is one row, not one line: the window can start halfway through a
wrapped line, and the row above the bottom has to be the row that was there
before.

8 rows tall: 4 rows of log, at width 4 (one column is spared), so six rows of text
scroll by two.

## a_long_input_wraps_into_more_rows_and_takes_them_from_the_log

A message longer than the line has to wrap inside the box instead of disappearing
off the right edge: the input is where the text gets edited, and text that cannot
be seen cannot be edited.

10 wide: a two-column prompt and one column spared, so seven of text. The frame is
header, five rows of log, footer, two input rows, help.

## a_typed_newline_starts_a_new_row

A typed newline is a hard break: the row after it starts a new line even where the
width would have kept going.

## the_input_box_stops_growing_and_scrolls_instead

The box stops growing: past its cap it scrolls, because an input that eats the
window hides the answer it is replying to.

A three-row log is the floor the input may not take from.

`layout_input` follows the cursor, so the last row is the one shown.
