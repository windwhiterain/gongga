# tui/panes.rs

## panes.rs

The events area is a list of *views* over one scrollback. The names come from the
command layer -- a session's name, or the name of the line the operator's own commands
live on -- and this file turns that list into rows.

The division is here because a view is a drawing fact once the lines are named: which
line belongs to which window is the command layer's decision (see
`docs/notes/cli-lib.md`), how tall a view is and what it scrolls is nobody else's.

## One log, many views

A line is held once and read by every view that can show it. That is what makes
switching `--split` a *view* change rather than a data loss: the merged view and the
per-session views are two readings of the same blocks, so what arrived while one was
on screen is already in the other.

The alternative that looks cheaper -- one `Log` per pane, and the merged view only
when unsplit -- was built first and measured on a real terminal: splitting the area
mid-conversation left every pane showing one row that started mid-sentence, because a
pane only ever held what arrived after it was created. The frames are in
`.dev/live/`, and the rule they bought is this section.

The cost is that a block is counted twice -- once in the merged ledger, once in its
window's -- so an append updates two row sums. Both are O(log blocks), and a frame
still reads only the rows it draws.

## Pane

Name, what kind of view it is, and its own `scroll`/`max_scroll`. There is no shared
scroll: one session's history is not another's, which is the point of splitting.

`last_height` is what makes a resize legible: `scroll` counts rows up from the newest
row, so the same number means a different piece of text once the pane is taller.

## Pane::anchor

`scroll` is a distance from the bottom, which is what makes "stay at the newest row"
free: appending text moves the bottom, and a scroll of 0 follows it. The price is that
the number means something else when the window changes height -- a pane that grew by
four rows shows four *older* rows above the same bottom, and the paragraph the
operator was reading slides down the screen while they watch. Reported as: scroll out
of the bottom, make the terminal taller, and the position moves.

So the height is remembered and the scroll is re-expressed when it changes, holding
`start = total - scroll - height` still. Away from the bottom that keeps the top row
where it was and puts the new rows *underneath* what is being read; at the bottom
(`scroll == 0`) nothing is re-expressed, because there growing is exactly what shows
more history. Content still arriving needs no counterpart: a new row at the end does
not move a scrolled view at all, which is the whole reason the number is measured from
the bottom.

`Kind` is which part of SPEC's window a pane is: the session list, a full pane, a
line group, or the command line. Only `Full` panes are laid out in the body's right
half; the line groups and the command line have areas of their own, so the body's
arithmetic never counts them.

## Panes::layout

One pane per announcement, in its order: the list, then the full panes, then the
line groups, then the command line. An empty command name is no command line at all,
which is what a frame of events alone asks for.

A relayout carries each surviving pane's scroll over. The announcement is sent on
every event that changes what is drawn -- a status, and the measurement that puts
numbers in a pane's title -- so a pane rebuilt from zero would jump to its newest row
every step, which is exactly what SPEC says a view scrolled away from the bottom must
not do. The scroll bound comes with it, because it belongs to the same view; the frame
re-clamps both against the height it actually has. `last_height` travels too: a
session joining the split makes every other pane shorter, and that is a height change
`Pane::anchor` has to see.

## Panes::pin and Panes::scroll_by

One pane's scroll, by name: SPEC's page keys act on the sticky session, not on
whatever the keyboard last pointed at.

The list is what is *shown*; a window that drops out of it keeps nothing of its own,
because it never held anything -- the log does. Re-announcing the same list is not a
no-op: the order comes from the list, and one path for "changed" and "unchanged" is
what keeps the order truthful.

The merged announcement carries the sessions it leaves out, and this is where the
log is told: split, a hidden session has no pane; merged, it has no lines. Both are
the same set from the same command, so the two cannot disagree.

## Panes::quota, Panes::chrome

The allowance row is the second thing here that comes from the announcement rather
than from the log -- the list is the first -- so it is held the same way: cloned by
`layout`, with no history behind it.

`chrome` exists so the frame's caller does not have to know which rows are
conditional: it is the one place that asks "is there a command window, how many
line groups, is there a reading", and `areas` sizes the frame from it.

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

Submitting a line pins every view to its newest row. The operator is about to cause an
answer; leaving a view parked three pages up means the answer appears off screen and
looks like nothing happened.

