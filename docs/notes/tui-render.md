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

SPEC's `hot-key` list, one entry per binding, in SPEC's order: six of them, the
ones the reminder may name.

`Shift+Delete` is the delete and `Alt+V` the paste. Both are SPEC's, so they are
on the row; the paste also arrives as a bracketed paste from a terminal that
pastes by itself, and the entry is about the key, which is what the row is for.

`Shift+↑ last cmd` is the newest entry, and the shortest way to say it: one key,
one thing it brings back. Its two neighbours on the row are about the view
(`Shift+↓`, `PageUp/PageDown`) and it is not -- the row reads as a list of keys,
not as a grouping, so the entries stand in SPEC's order rather than by topic.

The paste entry used to name two spellings, `Ctrl+V/Ctrl+Alt+V`, because SPEC
named two. Neither reaches a program inside VS Code -- the first is the
terminal's own paste, and xterm.js writes nothing at all for the second -- so the
row now says the one key SPEC binds, `Alt+V paste`: one entry, one binding, one
key. The measurement is in `docs/notes/tui-paste.md`.

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

Each row also carries the bytes of the text it holds: the rows are the text, cut,
and a selection is a range of bytes. Without the cut, a selection could not be
drawn -- or found by a click.

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

`bottom_at` is asked before `list_at` and `pane_at` in `Screen::session_at_row` for
that reason, and it only answers for a view that is actually scrolled: a hidden hint is
not a click target.

The list is one of the views it answers for, and it is the reason the order is
`bottom_at` first: the hint sits on the last row the list -- which draws one name per
row -- would otherwise hand to its own toggle. The list's hint names the list's own
pane, whose name is empty because it reads no window of the log, so pressing it pins
the list and toggles no session.

## list_window

The rows of the session list a frame shows: its tail, scrolled up by the list's own
offset.

SPEC's `(vertical scroll)` is on the list as it is on a window, so the list holds a
scroll and the wheel over it is the list's -- the same number, counted the same way,
as a window's. It was derived from the sticky session for a while, which read as a
list that could not be scrolled at all: a wheel over it moved nothing, and the note
that said the list had a scroll of its own was the only thing claiming otherwise.

The frame and the click read this one function from the same offset, so they cannot
disagree about which name sits on which row.

A consequence, said out loud because it is visible: the list no longer follows the
sticky session. The name a message will go to is marked by the frame around that
session's window, which is beside the list, and the list is a list of every session
-- scrolling it is what says which part of it is on screen.

## draw_header

The bar spans the row and is painted by the status's kind: cyan for the server's
facts, red for an error, green for a result. One colour for all three would make
"copy failed" and "3 session(s)" look equally urgent.

It carries two facts: whose lifetime the server shares (`own #1234`,
`own #1234 (handover)`, `not own #1234 (another window)`, `not own #1234 (no
owner)`) and the status. The first words are the answer to "does closing this
window stop my server", which is why the bar exists at all -- the four labels are
`tui-run.md`'s table, and `Server::label` is the only place they are spelled.

## draw_list

SPEC's session list, on the left of the body: a tree, one row per prefix of a name.

The rows themselves are `tui-tree.md`'s; what this function decides is their look. One
row reads as `○  [api]` expanded and `○ |[work]` folded: the status mark and its space,
the indent, the `|` a fold draws (and nothing when it draws none), the label. Indent and marker are painted cyan -- `structure`, the ink a window's rules
already use -- because they are the shape of the tree and not a fact about a session.
The label is bracketed only on a row that names a session; a row that is only a group
is the segment as typed. A header still says the whole name (`[work/api]`), so the two
areas never disagree about what a session is called: the list cuts a name into its
segments, the header spells it.

It used to hold only the sessions no other area drew, which made it the list of what
was hidden rather than the list of what exists -- and a session drawn `full` had its
name in two places. SPEC now gives the list every name and puts the toggle in the
click: a shown session is still a row, because that row is where it is hidden again.

Two facts, one column each, and each look means one of them:

- **the status mark**: `●` running (green), `●` stopped unseen (yellow), `✕` error (red,
  bold), `○` anything else (chrome) -- `status_mark`. It is what the announcement says
  the session is doing, which used to be announced and dropped, so "is it running?"
  could only be answered by reading that session's events. A group row carries the
  strongest mark under it (`tree::Mark`, error over stopped over running over idle), so
  a folded group still says whether something inside it needs attention.
- **the name's brightness**: bright when the session has a window beside the list, dim
  when it does not. The right-hand split states the same fact, said again where it is
  clicked.

The mark is *one* column whatever the word is, and that is the whole reason it is a
mark. A word costs the width of the widest one -- `running` is seven columns -- and the
list is a column of names that is read at a glance. The shape carries the state as well
as the colour does, so `running` and `error` are still distinct to a reader who cannot
tell green from red; `idle` is the state of a session that needs nothing, so it gets
the quietest look.

Rotating the glyphs to buy the width back is not on the table: a terminal draws one
upright glyph per cell, and `ratatui`'s cell is a symbol plus a style with no
orientation to set (there is no rotation anywhere in `ratatui-core`). Writing a name
vertically -- one character per row -- would trade width for height, and the list is
already the part of the window with the least height to give.

It does *not* say which session is sticky. The sticky one is necessarily shown, and its
window is framed (`pane_slots`) -- so a second mark here would be the same thing twice,
in the area that is about something else. The list used to carry the mark column `▸`
and a fill to the right edge for it; both went when the frame became the sticky mark.

A name is bracketed -- `● [main]` -- in the list, and `[main]` in the window's header:
one spelling in both areas, so a name cannot be mistaken for the first word of the
events beside it. `bracketed` is that spelling, and `list_width` adds it up per row:
`Row::marker_column` (the mark, its space and the indent), the two marker columns, the
label and, on a session, the two brackets -- then one column of air. Taking the widest
row rather than the widest name is what makes an indented label fit: the columns before
it are part of what the row costs. A folded row costs one column more, so a list whose
widest row is folded is one column wider than the same list unfolded.

The frame is also the only place the list's height is known, so `draw_list` writes
the bound back (`Pane::bound`, the same call a window makes in `draw_rows`) and clamps
the offset into it: a list that just lost a session, or a window that just grew,
lands inside its own rows before it is drawn. Every other reader -- the wheel between
frames, the click -- reads that bound instead of working it out again.

Scrolled away from its newest row, the list shows the way back to it: the same hint,
on the same condition, as any other view.

`list_at` is the same arithmetic from the other side, and it answers a `ListHit`: the
row's full name, and whether the click asked to fold -- the `|` of a folded row, or the
status mark of a group row, which is the only column left that is not text. A marker
folds what is under it, a name does what a name does, and one function answering both
is what keeps a click from folding the row it did not point at.

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
rows are laid out one row inside it, so the rules are drawn where the window is rather
than over it. Two rows of that column are what the box costs, and no column of it: the
frame is a rule above and a rule below, so a sticky window's rows are as wide as any
other's, and the rules say which window the keys scroll. A column too short for two
rules keeps its rows and is marked by `name_style` instead, because a border around
nothing is worse than a fill. `Slot::frame` is the box, and it is also what the hit
test reads: a click on a rule is a click on the session.

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

## item_row

Two inks, and which one a row gets is the question the row answers.

A mark is the one thing that says what a row *is* -- `▸` a call, `✓` what it
printed, `!` a failure -- so it keeps the kind's colour, and the table in `style_for` is
the palette of marks. Everything printed beside it is the one quiet gray: a call's
arguments, a result, the harness's remarks. Ten colours down the body of the log is a
screen where nothing stands out, and a palette exists so that something does.

`output` is the exception, and it is why the rest may go quiet: it is what the operator
came to read, so its rows are drawn as the markdown they print -- `markdown::colored_row`,
and `docs/notes/tui-markdown.md` for what a row can say. It carries no mark, so its colours
are spent on structure and none on the question of what kind of row it is.

## ruled_row

An opened item's box is two rules and the rows between them, and the item's label rides
on the top rule. The box has no sides to pay for, so its rules are as wide as the window
and every row of it starts at the window's left edge. The label keeps one column of air
and ends in at least one dash, which is what tells the top rule from a folded row at the
same width. A row of the body is drawn like any other row -- the kind's mark, then the
quiet gray -- and the log already wrapped it, so the renderer pads nothing on the right
and cuts nothing.

`set_render --box_edge` is deliberately absent from this module: those columns are drawn
exactly as the rest of the box, and the setting only decides where a wheel is the pane's
(`log::hit`). An earlier version inset every row here by that much and the box looked
like a narrower block floating in blank columns.

## shown_row

A row of a pane arrives two ways, and the difference is who decided how it is drawn.
`Shown::Styled` is an answer's row: the log holds the document's runs, and the frame
draws them as they are -- no mark, no gray, no re-reading of the text. `Shown::Text` is
every other row: the kind's mark in its colour, then the quiet gray.

The split is not a shortcut for the answer. It is what makes a break harmless: the ink
was decided before the width was, so a row a fold split carries the same runs on both
sides, and `ruled_row` -- which draws a box's rules and the label on its top one --
never has to know a document exists.

Styled rows are cloned per frame, as text rows are: the frame outlives the borrow of
the log it read, and a screenful of runs is cheap next to a redraw.

## draw_input

The box stops at its cap rather than growing forever, so a long message scrolls
inside it: the window follows the cursor instead of the cursor leaving the window.

The placeholder explains the empty box; it is not text, so it is only drawn where
the text would have started.

A row that exactly fills the width puts the cursor one past its last column, which
is outside the area: keep it on the edge.

The selected bytes are drawn reversed, and only they: what the window shows
highlighted is exactly what `Alt+C` copies.

## input_offset

Where a click in the input box lands, as a byte offset into the text.

The pointer's column is a display column, so it is walked back through the row's
characters until it finds the one that covers it; a character two columns wide is
entered at its own start. The offset is a character boundary, because the text is
cut at it.

## draw_completion

The window follows the selection. `move_selection` walks the whole list, so drawing
only the first screenful leaves the ninth candidate selected with nothing
highlighted -- and Tab still inserts it.
