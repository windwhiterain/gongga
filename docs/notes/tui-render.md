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

## style_for — one look per kind

Ten kinds of line, and no two look alike — colour *and* modifier, because a colour
that means two things means neither:

| kind | look | what it is |
|---|---|---|
| `System` | dark gray | the harness's own remarks |
| `Command` | light magenta | a line that was asked for: `/cmd` or `$ cmd --args` |
| `User` | light cyan | prose the operator typed |
| `Assistant` | white | the answer |
| `Reasoning` | dark gray, italic | the model thinking |
| `ToolCall` | yellow | a tool being called |
| `ToolOutput` | light blue | what it printed |
| `ToolError` | light red | what it complained |
| `Result` | green | a command's answer |
| `Error` | red, bold | something needs looking at |

`Command` exists because the echo used to be `System`: a reader scrolling back for
"what did I run" was looking for a colour that also meant "the harness is talking".

One colour is shared on purpose — `System` and `Reasoning` are both dark gray,
because both are asides; italic is what tells the model's thinking from the
harness's remark. `Reasoning` was `Gray` for one revision and that was wrong: on a
Windows console `Gray` renders as near-white, which is the answer's own colour, so
the thinking read as part of the reply.

`no_two_kinds_of_line_look_alike` walks every kind and fails on a repeat, so the
table above is checked rather than remembered.

## draw_bottom

SPEC's "show a go to bottom button", drawn on exactly the condition it names: the
view is away from its newest row. A button that is always there says nothing about
where the view is, so a frame at the bottom draws none.

It sits on the last row of the view it belongs to, right-aligned, on a coloured
strip so it reads as a control rather than as a line of the log. It says
`↓ bottom · Shift+↓` when the width allows it; over a narrow session list -- whose
columns are only as wide as its names -- it is the bare `↓`, one column, which lands
on the list's spare column instead of on a name.

## list_window

The rows of the session list a frame shows: the ones around the sticky session.

The list owns no scroll. SPEC gives it a vertical scroll, and the sticky session is
that scroll's only position: the window follows the name, so the row that says where
a message will go is never the one that scrolled away, and a click lands on a name
the operator can see. A name nobody knows -- before the first `set_session` -- pins
the window at the top.

The frame and the click read this one function with the same sticky session, so
they cannot disagree about which name sits on which row.

## draw_header

The bar spans the row and is painted by the status's kind: cyan for the server's
facts, red for an error, green for a result. One colour for all three would make
"copy failed" and "model x | 3 session(s)" look equally urgent.

## draw_list

SPEC's session list, on the left of the body: the sessions whose render mode is
`none`, one row each.

It used to be every announced session, and that was the second place every name was
said: a session drawn `full` was in the list *and* over its pane, `line` in the list
*and* on its row. SPEC gives each of the three modes one area, so the list is the
one left over. A name still comes back to the list by walking the modes -- a pane's
name to `line`, a line's to `none` -- so nothing is unreachable.

Two things follow, and both were reported from a real window: no name is drawn twice,
and every row of the list is painted the same way. The list used to colour a name by
the mode that session would be drawn as elsewhere (white for `full`, dim for `line`,
dark grey for the rest), which made one column look like three kinds of row.

The sticky session is marked by `name_style`: the mark column `▸`, and the same
highlighted patch its name gets in a pane header and at the start of a line group.
It is one state -- what the page keys scroll and where a message goes -- and one
look, wherever the name happens to be.

## name_style

The look of a session's name: the sticky one is black on cyan, every other name is
the area's own base style. The base differs on purpose -- a pane header is chrome and
stays dim, a line group's row is the newest event and stays the text colour -- but the
*state* does not: a header used to mark the sticky session with a dark grey patch
while the list and the line rows used cyan, so the same thing looked like two.

## draw_lines and group_slots

The sessions drawn as `line`, below the panes: one row per event, laid out exactly
like the panes -- `group_slots` is `pane_slots`' arithmetic, so a header each, the
rows shared out, and what is left over to the sticky session. The difference between
`full` and `line` is the height, not the picture: the same grey header with the
session's name on it, painted by `draw_group_header`, which both use.

A flat `height / groups` was wrong in the way that shows: the earlier groups took
rows the later ones still counted, and a group ended up drawn as blank lines.

## draw_headlines

One row per *event*, which is not one row per wrapped row. `Log::headlines` gives
one line per block: a break inside a block becomes a space and text past the width
is cut with `…`. Drawing a group from `Log::window` -- the wrapped rows -- showed a
sentence's tail and blank continuation rows, which is what "line sometimes turns
into an empty line" was.

The block's label rides on that row (`[peer] …`): a line group has no header per
block, so the row is the only place to say who is speaking.

## body

The body split into the session list and the panes beside it. The list is as wide as
its own content -- the mark, the longest of *its* names, one column of air -- and
never wider than half the window: the panes keep their share, because a pane with no
columns shows nothing.

It used to measure every announced session, which was right while the list held them
all. Now that the list is the `none` sessions, a pane's longer name no longer widens
the column it is not drawn in.

## draw_panes

The panes take the events area in announced order; each gets a header naming it and
the share of rows its neighbours do not use. The header only appears for a window
with a name, so the unsplit area -- whose one window is named nothing -- looks exactly
like it did before splitting existed.

The header carries the numbers SPEC gives a `full` pane -- context, tps, ttft -- read
from the announcement as the command layer spelled them, and then the note about a
pane the division hid. One title, parts joined by `·`, because two strings written by
two functions is how `main · 1 not shown · 1 not shown` happens.

## usage_note

The three numbers, and only on a `full` pane. SPEC's list row is a name, and a
`line` group's row is one event: putting a rate in either would be a fourth thing
that row says. A session that has measured nothing contributes no part at all, so
the title falls back to the name it always had.

## draw_command

One row, under the `active_quota` row and above the prompt, and only when the command
layer announced a command window. It is drawn by the same code as a pane's body, so
its rows carry the same colours and the same clipping.

## draw_quota

SPEC's `active_quota` row: one line above the command line, under the line groups,
painted by the kind the command layer gave it -- a reading is dim chrome, a provider
that could not be read is the error colour.

Above rather than below, because the command line is SPEC's `[{cmd, cmd_result}]`
ticker and the prompt under it is where the operator's attention sits: a balance is
read once in a while, a command's answer every time. It is also the order SPEC lists
them in.

No row exists before the first reading, so the frame is exactly what it was before the
feature for a server that has nothing to report, and for a provider whose roster entry
publishes no reader.

## Chrome

The rows that exist only when something is drawn in them -- the command line, the
line groups, the quota row -- as one value, because SPEC's rows are positional and
two bare booleans next to each other is a call that swaps them silently.

## areas

The layout a frame is made of, in one function, because three callers need it: the
renderer, the copy (`Screen::visible_rows`) and the mouse hit-test. `CHROME_ROWS`
plus the command row plus the input's own rows is the whole arithmetic; a second
place that computed it is how a copy ends up including rows the screen never drew.

## pane_slots

SPEC's horizontal split: the panes sit side by side, so what is shared out is the
*width*, and what the division leaves over goes to the sticky session -- the operator
is reading that one, so it should be the wider column.

Each pane gets a header on the first row of its own column and the rest of the height
under it, so a header is per column rather than a per-pane band across the window. A
pane that cannot get a column is not drawn and counts towards `hidden`, which the
last drawn header says out loud -- silently dropping a session's window is how a user
decides the log is broken.

A header costs one row of the body's height, once, however many panes there are.

## pane_at

The cell, not the row. With the panes side by side, one row of the window holds
several of them, so a hit test that asked only "which pane covers row N" answered
with the leftmost one for every click -- and the right-hand session's name could not
be pressed at all. `covers` checks both coordinates, and a header is a cell like any
other, so a click on a pane's title names that pane.

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

## the_log_window_shows_the_newest_rows_above_the_prompt

Whole-path check without a terminal: layout, window, and the rows the bottom of the
log actually shows. The TUI cannot be run in a test, but "the newest line is the
one above the prompt" can be.

10 rows tall: the header, the body, and the prompt -- and the pane's own header takes
one of the body's rows, which is why the log is not eight.

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
the header, the log, and two input rows.

## a_typed_newline_starts_a_new_row

A typed newline is a hard break: the row after it starts a new line even where the
width would have kept going.

## the_input_box_stops_growing_and_scrolls_instead

The box stops growing: past its cap it scrolls, because an input that eats the
window hides the answer it is replying to.

A three-row log is the floor the input may not take from.

`layout_input` follows the cursor, so the last row is the one shown.

## a_full_panes_title_carries_what_the_last_request_measured

The three numbers appear on the `full` pane's header, a session that has measured
nothing gets the bare name, and the `line` group's one row carries none of them --
which is the whole of SPEC's rule about where they live.

## a_relayout_leaves_every_pane_where_it_was_scrolled

The announcement arrives on every step of a turn, so it is not a reason to move a
view: a pane that kept its rows keeps its place in them.

## the_list_is_the_sessions_no_other_area_draws

The rule, measured on a frame: a `full` and a `line` session stay out of the list,
the two `none` names are its first two rows in the announcement's order, and their
cells carry one style between them.

## the_sticky_session_is_marked_the_same_way_in_every_area

One name read at two places in one window -- the line group's row and a pane's
header -- and the same black-on-cyan cell at both, which is what `name_style` is
for.

## the_quota_row_sits_above_the_command_line

The row with a reading is one row, SPEC's order holds (quota, then cmd, then prompt),
and with no reading the prompt sits straight under the command line.

## an_unreadable_quota_row_is_drawn_as_an_error

The same row, painted by the kind the command layer chose: a failed reading is
visible without a second kind of row.
