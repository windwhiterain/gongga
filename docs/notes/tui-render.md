# tui/render.rs

## CHROME_ROWS

Rows the layout spends on everything that is not the body or the input: the header.
The input's own height varies with its text, so it is asked for separately (see
`input_rows`), in exactly these terms -- it may not take a row the header has.

## PREFIX

The prompt on the first row, and the padding continuation rows line up behind.

## MAX_INPUT_ROWS

The most rows the input box grows to before it scrolls.

A message is prose, not a file: past a screenful of it, what the operator needs
to see is the answer they are replying to, so the box stops growing.

## HELP

SPEC's `hot-key` list, one entry per binding, in SPEC's order: five of them, the
ones the reminder may name.

`Shift+Delete` is the delete and `Ctrl+V` the paste. Both are SPEC's, so they are
on the row; `Ctrl+V` also arrives as a bracketed paste from a terminal that
pastes by itself, and the entry is right either way.

Nothing else goes on the row even though this window binds more: the operator
reads a reminder as the whole keyboard, and SPEC is the source of truth for which
keys exist. `Alt+Q` still quits, `Tab` still completes — they are in
`docs/notes/tui-lib.md` and in DESIGN, not on the row. The first version of the
row listed every binding the handler knows, which was wrong in the direction that
costs more: a key that is not a SPEC key looked like one.

## HELP_SEP

What sits between two entries on one row: ` · `. A row that starts with an entry
carries no leading separator, so a wrapped reminder reads as a list of keys.

## help_rows

Packs the entries into the rows a window `width` columns wide allows, and the
layout asks here for how many rows the reminder will take.

An entry is never split across two rows: a row that cannot hold the next one ends
before it, and an entry wider than the whole window keeps a row to itself, where
the terminal cuts it. Splitting `PageUp/PageDown scroll` after `PageUp/` would
turn one key into two rows that read as two keys.

## areas

Seven rects, and the reminder is the last of them by construction: `help_rows`
says how many rows it needs at this width, those are cut off the bottom first,
and the six above are laid out in what is left. A window too short for its parts
therefore loses a pane row, not the keys — the solver returns whatever it likes
when the constraints cannot all hold, and "the reminder is the last rows" has to
survive a three-row terminal.

The count is a function of the width, so a resized window re-lays out the
reminder: 140 columns fit it on one row, 80 on two, and a 20-column terminal
gives each entry its own.

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

It sits on the last row of the view it belongs to, right-aligned, painted black on
cyan like every other mark this window draws, so it reads as a control rather than
as a line of the log. It says `↓ bottom · Shift+↓` when the width allows it; over a
narrow pane it is the bare `↓`, one column, which still fits beside the row's text.

The last row of a scrolled view is the one it covers: the hint is drawn *over* that
row rather than under the text, and the row is what a click there would otherwise
have named.

## bottom_hint, bottom_rect and bottom_at

One rect, three readers. `bottom_hint` decides the text and the cell it covers,
`draw_bottom` paints it, and `bottom_at` answers a click with it: a button the
pointer cannot press is a picture of a button, and a click that lands on one used to
fall through to the pane under it and set that session sticky -- so the way back to
the newest row asked for a command instead.

`bottom_at` is asked before `pane_at` in `Screen::session_at_row` for that reason,
and it only answers for a pane that is actually scrolled: a hidden hint is not a click
target.

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
"copy failed" and "3 session(s)" look equally urgent.

## draw_list

SPEC's session list, on the left of the body: every session, one row each.

It used to hold only the sessions no other area drew, which made it the list of what
was hidden rather than the list of what exists -- and a session drawn `full` had its
name in two places. SPEC now gives the list every name and puts the toggle in the
click: a shown session is still a row, because that row is where it is hidden again.

The name's brightness says one thing: **bright when that session has a window beside
the list, dim when it does not**. That is the same fact the right-hand split states,
said again where it is clicked.

The word after the name is what the announcement says the session is doing -- `idle`,
`running`, `error` -- and it is painted by that word: `running` green, `error` red and
bold, everything else chrome (`status_style`). It used to be announced and dropped, so
"is it running?" could only be answered by reading that session's events. Two words
earn a colour because they are the two that change what an operator would do next;
`idle` is the state of a session that needs nothing, so drawing attention to it would
be drawing attention to nothing.

The words are padded to the widest one the list draws (`Panes::status_width`), so they
start on one column down the list and `list_width` charges the column for them. A list
of two names and one status is not worth two different alignments.

It does *not* say which session is sticky. The sticky one is necessarily shown, and its
window is framed (`pane_slots`) -- so a second mark here would be the same thing twice,
in the area that is about something else. The list used to carry the mark column `▸`
and a fill to the right edge for it; both went when the frame became the sticky mark.

A name is bracketed -- `[main] running` -- in the list, and `[main]` in the window's
header: one spelling in both areas, so a name cannot be mistaken for the first word of
the events beside it. `bracketed` is that spelling, and `list_width` counts the two
columns it adds, the status and one of air.

`list_window` still centres the list on the sticky session even though the row no
longer says so: which rows are on screen is a scroll position, not a mark, and the
session the operator is reading is the useful thing to centre on.

## name_style

The sticky mark itself: black on cyan, one row. What is left of it after the list
stopped using it is the way back to the newest row (`draw_bottom`), and the fallback
fill on a header whose window is too small to frame.

It was a *row*, not a word -- `fill_row` padded what was left of the area with it,
because a patch that stops where the text stops reads as a highlighted word. That is
gone with the last caller: a header is one line of text in its own band, and the frame
is what marks a window.

The base style differs on purpose -- the list is dim chrome, a window's header is dim
too -- but the state does not: the same session used to be a dark grey patch in a
header and cyan in the list, so the same thing looked like two.

A window in the split is the exception, and it is a different mark rather than another
colour: it is framed (see `pane_slots`), so its rows keep the look of the kind of event
they hold. Filling its rows would paint over the colours the operator reads them by.

## draw_lines and draw_headlines

Gone with SPEC's `line` render mode. A session is either shown -- a header and its
events -- or a row in the list, so there is no second way to draw one, and no
headline row to draw for it. `Log::headline` and its one-line-per-event flattening
went with it: a row that is nobody's window is a row nothing asks about.

## body

The body split into the session list and the windows beside it. The list is as wide
as its own content -- the mark, the longest name, one column of air -- and never
wider than half the window: the windows keep their share, because a column with no
columns shows nothing.

It measures the list's names, which are now every announced session, so the column is
exactly as wide as the widest name it draws.

## draw_panes

The windows take the events area in the announcement's order, side by side; each gets
a header naming it and the share of columns the others do not use. The header only appears
for a window with a name, so the unsplit area -- whose one window is named nothing --
looks exactly like it did before a second session existed.

A slot with a `frame` has its box drawn first, in the mark's colour and with no
background: the border is the mark, so a frame never meets a filled row. The header is
told whether the row is filled rather than which session is sticky, which is what lets
one function paint a pane in the split and the fallback case of a pane too small to
frame.

The header carries everything SPEC gives a shown session -- the measurements and the
session's own tuning -- read from the announcement as the command layer spelled them,
and then the note about a column the division hid. One title, parts joined by `·`,
because two strings written by two functions is how `main · 1 not shown · 1 not shown`
happens.

## pane_note

Everything SPEC's header carries, and only in a header. SPEC's list row is a name:
putting a rate or a provider there would be a second thing that row says. A session
that has measured nothing and named nothing contributes no part at all, so the header
falls back to the name.

## pane_header_rows, wrap_words

A title that does not fit its column is drawn over several rows instead of being cut.
`pane_header_rows` is the one place that decides where it breaks, and it is asked
twice for one frame -- once by `pane_slots` to reserve the rows, once by
`draw_pane_header` to fill them -- so the reservation and the drawing cannot
disagree. Two wrapping rules (the layout using one, ratatui's `Wrap` using another)
was exactly the bug that followed the first attempt: the header reserved four rows and
needed five, and the last field of the session's title was silently gone.

The name is bracketed -- `[main]` -- in both areas, so a name reads as a name rather
than as the first word of the events beside it. That is also why the client stopped
prefixing `[session] ` to the text: one place says who is speaking.

A header never takes the column's last row: a title that needs more room than the
column has is cut by the column's own height, and `pane_slots` keeps one row for the
events, which are what the window is for.

## draw_command

One row, under the `active_quota` row and above the prompt, and only when the command
layer announced a command window. It is drawn by the same code as a pane's body, so
its rows carry the same colours and the same clipping.

## draw_quota

SPEC's `active_quota` row: one line above the command line, under the windows,
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

The rows that exist only when something is drawn in them -- the command line and the
quota row -- as one value, because SPEC's rows are positional and two bare booleans
next to each other is a call that swaps them silently.

## areas

The layout a frame is made of, in one function, because three callers need it: the
renderer, the copy (`Screen::visible_rows`) and the mouse hit-test. `CHROME_ROWS`
plus the command row plus the input's own rows is the whole arithmetic; a second
place that computed it is how a copy ends up including rows the screen never drew.

## pane_slots

SPEC's horizontal split: the windows sit side by side, so what is shared out is the
*width*, and what the division leaves over goes to the sticky session -- the operator
is reading that one, so it should be the wider column.

Each window gets a header on the first row of its own column and the rest of the height
under it, so a header is per column rather than a band across the window. A column that
cannot get a cell is not drawn and counts towards `hidden`, which the last drawn header
says out loud -- silently dropping a session's window is how a user decides the log is
broken.

A header costs one row of the body's height, once, however many windows there are.

The sticky window's shape is one variant: its column gets a `frame` and its header and
rows are laid out one column and one row inside it, so the box is drawn where the window
is rather than over it. Two columns and two rows of that column are what the box costs;
a column too small for a border keeps its rows and is marked by `name_style` instead,
because a border around nothing is worse than a fill. `Slot::frame` is the box, and it
is also what the hit test reads: a click on the border is a click on the session.

## pane_at, header_at

The cell, not the row: with the windows side by side, one row of the window holds
several of them, so `covers` checks both coordinates and `pane_at` is one function the
wheel and the click share.

`pane_at` answers for a window's whole column -- header, rows and box -- because the
wheel over any of them scrolls that window. `header_at` answers for the header alone,
because SPEC's click makes a session sticky from its name and figures, while a click in
the events is a click on text the operator is reading. The earlier shape had one hit
test for both jobs and a second click that walked the render modes; the modes are gone
and the two gestures now have their own answers.

## draw_rows

One pane's rows, and the bound and scroll it writes back.

The log hands back exactly the rows that fit, already wrapped and borrowed from
its own buffers, so this walks a screenful and nothing else. The scroll bound
comes back with them because only here is the window height known, and the key
handler needs it between frames.

One column short: a row that exactly fills the width leaves the terminal free to
wrap it again on its own, which double-spaces every full line.

Clamping here as well as in the key handler covers the cases the handler cannot
see: a window that just grew, or a line set that just shrank.

`Pane::anchor` is asked before the window is read, and with the pane's own row
count: the height alone cannot tell a shorter window from a longer log, and a
scroll offset counted from the newest row means nothing without both. Skipping it
is how a scrolled view came to follow the log instead of holding its top row.

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

## a_windows_header_carries_what_the_last_request_measured

The three numbers appear on the shown session's header, a session that has measured
nothing gets the bare name, and a hidden session has no header to carry them -- which
is the whole of SPEC's rule about where they live.

## a_relayout_leaves_every_pane_where_it_was_scrolled

The announcement arrives on every step of a turn, so it is not a reason to move a
view: a pane that kept its rows keeps its place in them.

## the_list_is_bright_for_the_sessions_it_draws_and_names_what_each_is_doing

Both answers, read off one frame: every announced name is a row in the announcement's
order, its colour says whether it has a window (white for shown, dark grey for hidden,
even for the session that is sticky), and the word beside it says what it is doing --
chrome for `idle`, green for `running`, red for `error`.

## the_sticky_session_is_a_frame_and_the_list_says_only_what_is_shown

One session, one mark, one place: the four border corners of the sticky session's
column in the mark's colour, while the cell just inside its header keeps the background
it was drawn with -- and the list's row for that same session carries no fill. The list
answers a different question (shown or not), so the two never say the same thing twice.

## the_quota_row_sits_above_the_command_line

The row with a reading is one row, SPEC's order holds (quota, then cmd, then prompt),
and with no reading the prompt sits straight under the command line.

## an_unreadable_quota_row_is_drawn_as_an_error

The same row, painted by the kind the command layer chose: a failed reading is
visible without a second kind of row.

## a_long_pane_title_wraps_instead_of_being_cut

A title wide enough to need several rows, drawn at a real pane's width: every field is
on screen somewhere in the header, the body starts under the last header row, and a
one-row body keeps its row with no header at all.

## a_scrolled_view_holds_its_top_row_when_a_line_arrives

Whole-path, the other half of the same invariant: draw, scroll up five rows, add two
lines *under* the window, draw again, and the top row is the same text. Without
`last_rows` in `Pane::anchor` the window follows the log, which is the direction SPEC
forbids for a view that was scrolled away from the bottom.

## a_taller_window_keeps_what_a_scrolled_view_was_reading

Whole-path: draw, scroll up five rows, draw the same view at a taller terminal, and
read the top row back. It is the same row, the four new rows are underneath it, and a
view at the bottom is still at the bottom. Removing `Pane::anchor` from `draw_rows`
makes this fail with the row four further up (`L26` for `L30`), which is exactly the
symptom the operator reported.
