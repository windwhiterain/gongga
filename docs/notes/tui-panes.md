# tui/panes.rs

## panes.rs

The events area is a list of *windows* over one scrollback. The names come from the
command layer -- a session's name, or the name of the line the operator's own commands
live on -- and this file turns that list into rows.

The division is here because a view is a drawing fact once the lines are named: which
line belongs to which window is the command layer's decision (see
`docs/notes/client-lib.md`), how tall a view is and what it scrolls is nobody else's.

## One log, many windows

A line is held once and read by the window it belongs to. That is what makes showing a
session a *pane* change rather than a data loss: the blocks are already in the log, so
what arrived while it was hidden is on screen the moment it has a pane.

The alternative that looks cheaper -- one `Log` per pane -- was built first and
measured on a real terminal: splitting the area mid-conversation left every pane
showing one row that started mid-sentence, because a pane only ever held what arrived
after it was created. The frames are in `.dev/live/`, and the rule they bought is this
section.

Holding the blocks once means holding them at one width, and the width of a block is
the width of the pane that draws it. So the log keeps a ledger *per window*, each with
its own laid-out width (`docs/notes/tui-log.md`), and a frame that asks two panes at two
widths lays each block out once and then reads rows. The merged view that used to be a
third reading is gone: no frame drew it, and it is what made one width look like a fact
about the log.

## Pane

Name, what kind of view it is, and its own `scroll`/`max_scroll`. There is no shared
scroll: one session's history is not another's, which is the point of splitting.

`scroll` counts rows up from the newest row, so the same number means a different
piece of text once the view changes shape: `last_height` and `last_rows` are what the
last frame drew, and `Pane::anchor` needs both to know which row the operator was
actually reading.

## Panes::begin_frame, end_frame, evict

A frame says which windows it drew, and a window nobody drew holds no bodies: its
blocks stay in the index with their rows and go back to the journal. `draw_rows` also
hands its own visible row range to `Log::evict`, so the answer is per pane -- the rows
above and below what is on screen are what memory stops paying for.

Nothing is lost and nothing is a budget: the bound is the render itself, and a window
that comes back into view reads its page again.

## Pane::anchor

`scroll` is a distance from the bottom, which is what makes "stay at the newest row"
free: appending text moves the bottom, and a scroll of 0 follows it. The price is that
the same number names a different row as soon as anything else changes -- a pane that
grew by four rows shows four *older* rows above the same bottom, and a line arriving
under a scrolled view pushes that view one row up. Both were reported from a real
window: scroll out of the bottom, and the paragraph slides away while it is being
read.

So the height *and* the row count are remembered, and the scroll is re-expressed on
every frame, holding `start = rows - scroll - height` still. Away from the bottom that
keeps the top row where it was and puts the window's new rows -- taller window, or new
text -- underneath what is being read; at the bottom (`scroll == 0`) nothing is
re-expressed, because there growing is exactly what shows more history.

The two changes are one formula on purpose. Written as two, each compensating one
case, the taller-window case and the arriving-row case disagree about `last_height`
and the view drifts by the difference; as one, a frame at a new shape is a frame at a
new shape.

`Kind` is which part of SPEC's window a pane is: the session list, a shown session, or
the command line. Only `Shown` panes are laid out in the body's right half; the command
line has a row of its own, so the body's arithmetic never counts it.

## Panes::layout

One pane per announcement, in its order: the list, then the shown sessions, then the
command line. The two orders are the layout's and they are not the same one -- the
list by the turn that ended last, the split by when each session was made -- so a
finished turn moves a row and never a pane. An empty command name is no command line
at all, which is what a frame of events alone asks for.

A relayout carries each surviving pane's scroll over. The announcement is sent on
every event that changes what is drawn -- a status, and the measurement that puts
numbers in a pane's title -- so a pane rebuilt from zero would jump to its newest row
every step, which is exactly what SPEC says a view scrolled away from the bottom must
not do. The scroll bound comes with it, because it belongs to the same view; the frame
re-clamps both against the height it actually has. `last_height` and `last_rows`
travel too: a session joining the split makes every other pane shorter, and that is a
shape change `Pane::anchor` has to see -- a fresh pane with a row count of zero would
otherwise read as "the whole log is new" and jump to the top of it.

## Panes::list_rows, row_of and toggle_row

The list's rows are the projection in `tui-tree.md`, rebuilt here and nowhere else.
`layout` rebuilds them because the announcement is what they are a reading of;
`toggle_row` rebuilds them because a click on a marker is the other thing that changes
their shape.

Rebuilding on the click rather than in the frame is what keeps the two readers in step:
the frame draws `list_rows`, and the click is tested against the same rows, so a click
one frame after a fold lands on what that frame drew. A frame that folded would draw
one shape and hit-test another -- the click would fold the wrong row, and the report
would be "the marker does nothing".

`folded` holds full names, and `row_of` is how a caller asks whether a row names a
session: `session_at_row` answers with a name, and only the row knows whether the name
is a session or just a group.

## Panes::is_shown and Panes::mark_unseen

Two questions about one session: is it drawn beside the list, and did it stop while it
was not. The first is the layout's own answer -- a session with a `Shown` pane is
drawn, and `Shown` is what `set_render_session` writes -- and the second is what the
window remembers, because the stop is told once and the layout that follows says
nothing about it.

`unseen` is a view, like `folded`: a name stays until the session is shown again, and a
name the server has forgotten stays as one string that matches nothing. It is cleared in
`layout`, where the announcement says which sessions are drawn now -- a session that is
shown again is seen again, and its mark goes back to what the session is doing. The rows
are rebuilt there too, because the mark is drawn from them and not beside them (`tui-tree.md`).

## Panes::forget

The one thing here that empties the log, and it is not a layout change: the command
layer says the session was deleted (`Sink::forget`), and that name is one the server
may hand out again. `Panes::layout` cannot do it -- a hidden session keeps its lines on
purpose (see `tui-log.md`) -- so an announcement that stops drawing a name says nothing
about whether the session still exists.

The pane goes with the blocks. A session created under the deleted one's name is a new
pane, and it starts at its own newest row instead of inheriting the scroll of a view
nobody can see any more.

## Panes::pin, Panes::scroll_by and Pane::scroll_by

One pane's scroll, by name: SPEC's page keys act on the sticky session, not on
whatever the keyboard last pointed at. The arithmetic is `Pane::scroll_by`, one
clamp against the bound the last frame wrote, so a window and the list cannot drift
apart in how far a notch takes them.

`Panes::scroll_list_by` is the same call on the list. The list is a pane like the
others -- SPEC's `(vertical scroll)` is on it -- and it is the one pane whose rows do
not come from the log, so `Panes::list` finds it by kind rather than by name. Its name
is empty: a name is a window of the log, and the list reads the announcement instead.

The list is what is *shown*; a window that drops out of it keeps nothing of its own,
because it never held anything -- the log does. Re-announcing the same list is not a
no-op: the order comes from the list, and one path for "changed" and "unchanged" is
what keeps the order truthful.

The announcement carries the sessions it leaves out as well, and a session that is
not in the shown set has no pane -- which is the whole of "not drawn", since a pane is
the only thing that reads a window.

## Panes::quota, Panes::chrome

The allowance row is the second thing here that comes from the announcement rather
than from the log -- the list is the first -- so it is held the same way: cloned by
`layout`, with no history behind it.

`chrome` exists so the frame's caller does not have to know which rows are
conditional: it is the one place that asks "is there a command window, is there a
reading", and `areas` sizes the frame from it.

## The command view

`cmd` and `cmd_result` are drawn on a line of their own, under the `active_quota` row
and above the prompt, and every block in it is one row whatever `block_size` says: it
is a ticker of what the operator asked and what came back, not a place to read a
transcript. Its one row cannot scroll, so `Alt+Y` copying it takes that row alone.

A command that named a session is *not* drawn there: its echo, its rows and its
result line are read in that session's view, where the transcript they came from already
is. The line keeps the commands that named nobody -- `set_event_ui`, `cancel`, a
refusal.

## Panes::pin_newest

Submitting a line pins every view of the log to its newest row. The operator is about
to cause an answer; leaving a view parked three pages up means the answer appears off
screen and looks like nothing happened.

The list is not one of them. A submitted line appends to a session, never to the names,
so pinning the list would be a scroll nobody asked for -- SPEC's "if scrolled from
bottom, do not auto scroll" applied to the one view that has an offset and no new
rows to follow.

