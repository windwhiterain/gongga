# tui/render.rs

## CHROME_ROWS

Rows the layout spends on everything that is not the log or the input: the header
and the sticky line. The input's own height varies with its text, so it is asked
for separately (see `input_rows`), and the line groups get one row each.

## PREFIX

The prompt on the first row, and the padding continuation rows line up behind.

## MAX_INPUT_ROWS

The most rows the input box grows to before it scrolls.

A message is prose, not a file: past a screenful of it, what the operator needs
to see is the answer they are replying to, so the box stops growing.

## draw

The log borrows the screen mutably: it is where the scroll offset gets clamped
against a height that only the renderer can compute.

## draw — what it returns

The input area, as this frame laid it out. The caret and the post-typing erase both
need that box, and a second computation of it is how the two come apart: the terminal
can be resized between them, and then a cursor move or an erase lands on a row the
frame drew as something else.

## caret

Where the caret goes for this input in this area. One function, so the frame that
draws the box and the window that re-anchors the caret cannot disagree about it.

## input_ends

Where each visible row of the box stops being text: a column to erase from, on the
row it belongs to. Every column named is one the frame drew blank, which is what
makes the erase safe -- see `docs/notes/tui-lib.md`.

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

## draw_list

SPEC's session list, on the left of the body: one row per session the command layer
announced, whatever its render mode -- the list is how a mode gets changed, so
hiding a session from it would hide the way back. The sticky session is marked with
`▸` and a highlighted row, which is also the row the keyboard scrolls.

## draw_lines

The sessions drawn as `line`, below the panes: one row per event, shared out
between the groups. This is a ticker of what each session is doing, not a place to
read a transcript -- the whole text is still in the log, one `set_render_session`
away.

## draw_stickys

The sticky arguments, and the render modes that are not the default, on one row.
A session drawn as `none` has to be visible somewhere, or the log reads as broken.

## body

The body split into the session list and the panes beside it. The list keeps
`LIST_WIDTH` unless the window is too narrow to leave the panes any columns -- then
the panes keep their share, because a pane with no columns shows nothing.

## draw_panes

The panes take the events area in announced order; each gets a header naming it and
the share of rows its neighbours do not use. The header only appears for a window
with a name, so the unsplit area -- whose one window is named nothing -- looks exactly
like it did before splitting existed.

## draw_command

One row, between the footer and the prompt, and only when the command layer
announced a command window. It is drawn by the same code as a pane's body, so its
rows carry the same colours and the same clipping.

## areas

The layout a frame is made of, in one function, because three callers need it: the
renderer, the copy (`Screen::visible_rows`) and the mouse hit-test. `CHROME_ROWS`
plus the command row plus the input's own rows is the whole arithmetic; a second
place that computed it is how a copy ends up including rows the screen never drew.

## pane_slots

Equal shares, with what the division leaves over going to the focused pane: the
operator is reading that one, so it should be the taller one. A pane that cannot get
a row is not drawn and counts towards `hidden`, which the last drawn header says out
loud -- silently dropping a session's window is how a user decides the log is broken.

Headers give up before rows do: if the area cannot cover a header plus one row per
pane, the headers go first, because a pane showing nothing is not a pane at all.

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
