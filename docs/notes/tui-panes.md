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

## Panes::layout

The list is what is *shown*; a window that drops out of it keeps nothing of its own,
because it never held anything -- the log does. Re-announcing the same list is not a
no-op: the order comes from the list, and one path for "changed" and "unchanged" is
what keeps the order truthful.

## MERGED

The merged view is filed under the empty name, which no session can have: a session
name comes from the server. It is the pane the keyboard starts on, and the only pane
when the area is not split.

## The command view

`cmd` and `cmd_result` are drawn on a line of their own, between the footer and the
prompt, and every block in it is one row whatever `block_size` says: it is a ticker of
what the operator asked and what came back, not a place to read a transcript. Its one
row cannot scroll, so `Alt+Y` copying it takes that row alone.

A command that named a session is *not* drawn there: its echo, its rows and its
summary are read in that session's view, where the transcript they came from already
is. The line keeps the commands that named nobody -- `set_event_ui`, `cancel`, a
refusal.

## Panes::pin_newest

Submitting a line pins every view to its newest row. The operator is about to cause an
answer; leaving a view parked three pages up means the answer appears off screen and
looks like nothing happened.

