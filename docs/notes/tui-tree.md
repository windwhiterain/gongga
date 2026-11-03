# tui/tree.rs

## Why a tree at all

SPEC gives a session a `/`-separated name, and the list was a column of those names.
Every name drawn whole means the shape a person chose -- `work/api`, `work/web`,
`work/lib` -- is invisible: three rows that differ in their last four characters,
and no way to fold away the half of the list that is not being read. A tree says the
same thing in the same width: each name becomes its segments, and each segment is
its own row.

## Row

One row of the list. `name` is what the row stands for, `label` is what it draws --
the last segment. The two differ on purpose: the list draws `api` under `work`, and a
command, a click or a window header still names `work/api`.

`session` and `group` are two facts about one row, because a name can have both. A
session called `work` and a session called `work/api` draw one row for `work` that is
a session *and* a group: a click on its name does what a click on a name does, and a
click on its marker folds what is under it. Splitting them into two rows would be the
list lying about how many sessions exist, and dropping either would make one of the
two gestures unreachable.

`mark` is the strongest status of everything under the row, and `Mark`'s order is
that strength: error, stopped, running, idle. A folded group therefore still says
whether something inside it needs attention -- the one thing a fold is expected to
hide and must not.

`stopped` sits below `error` and above `running`: it is a session that finished while
the operator was not looking at it, which asks for a look, but it is not a failure and
it is not work in progress. A group that holds both a running session and one that
stopped unseen says `stopped`, because the one that needs the operator is the one that
is done.

## marker, marker_column, indent

The marker is **one** column and it exists only while the row is folded: `|lab` folded,
`lab` expanded. The column is not held open in the expanded case -- a column reserved
for a gesture is a column the names never get, and the list is the narrowest part of the
window that already spends two columns per level on indent.

That cost one thing the reserved column had: the `|` of a folded row is also the only
place that could unfold it, so an expanded group has nothing to click. The name is not
free either -- on a row that names a session it toggles the window -- which leaves the
status mark, the one column on the row that is not text. `list_at` therefore answers
`fold` for the `|` of a folded row *and* for column zero of any group row, and the two
targets are the same decision: whether this click folds the row.

The consequence is a one-column shift: a folded row's label sits one column right of the
row below it. That is what `|lab` over `lab` reads as, and it is where the space was
saved -- the alternative, a blank in every expanded row, keeps the labels in one column
by paying a column on every row of the list.

The indent is what says whose child a row is: two columns per enclosing group, spaces,
painted as structure. It is not a marker and no click reads it.

The column is not `2 + 2 * depth` by accident -- it is the status column and its space
(two, counted from the list's left edge), then the indent, which is two columns per
enclosing group.

## rows

The projection: one pass over the announcement, building nodes keyed by full name,
then a depth-first walk that skips a folded node's children.

The unseen set is the third thing the projection reads: a session that stopped while it
was not drawn is marked `stopped` instead of `idle`, so the list says that something
finished where the operator was not looking. An error keeps its own mark -- the failure
is the stronger fact, and it is already the loudest mark in the column (`mark_of`).

Siblings keep the announcement's order -- newest session first, as the store lists
them -- which makes a group appear where its first session is. The alternative,
sorting siblings by name, would keep the tree's rows still while sessions come and go;
it was not taken because the list's order is how a person finds the session they just
created, and the tree is a reading of that list, not a second one.

The walk is a stack, not recursion: depth is whatever the operator typed, and a
hundred-segment name must not be a stack overflow in the frame that draws it. A
segment that is empty never reaches here -- `session::checked_name` refuses it -- and
if one did, it would be a row drawn as nothing rather than a panic.

A folded node keeps its own row and drops its children; nothing is removed from
`nodes`, so unfolding costs no data and `folded` holding a name that no longer exists
costs one string that matches nothing.

## Folding is a view

The folded set lives in `Panes`, beside the scroll offsets, and a click on a marker
rebuilds `list_rows` immediately -- not in the frame. A frame is drawn from these
rows, and the click reads the same rows, so the shape the operator folded is the shape
the next click is tested against.

It is not written down anywhere: like an opened `abstract`, it is what the window is
doing, not what the server holds. Relaunching the client shows every group unfolded.
That was chosen over a new client command (`cli.toml` entry) because a name can be
folded from a click only, the state is one keystroke from being remade, and a stored
one raises a question with no good answer -- what does a fold mean for a group whose
sessions the server has since deleted?
