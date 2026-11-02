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
that strength: error, running, idle. A folded group therefore still says whether
something inside it needs attention -- the one thing a fold is expected to hide and
must not.

## marker, marker_column, indent

The marker is **one** column: `|` when the row is folded, a blank otherwise. A blank,
not nothing -- the column exists on every row, folded or not, group or not, so a label
starts at the same column as its siblings' and as the rows above it. Without it, `|lab`
would push `lab` one column right of `[work]` beside it, and the tree would read as
misaligned for whichever row happened to be folded.

It was two columns of twisty (`▾ ` and `▸ `) before that, and the width was the
complaint: two columns per row, on the part of the window that is narrowest and whose
names are already cut into segments. One column of `|` says the same thing -- a fold is
the one state a blank column cannot show by itself -- and the indent already says whose
child a row is, so a twisty was restating the indent.

A folded row is therefore `|lab` and an expanded one is ` lab`, with the label in the
same column either way: the glyph sits in the column the name would otherwise start in,
which is what makes a fold visible without moving the text.

The column is not `2 + 2 * depth` by accident -- it is the status column and its space
(two, counted from the list's left edge), then the indent, which is two columns per
enclosing group.

The frame and the click read `marker_column` from this side, so a click cannot land on
the marker of one row and fold another one: `render::list_at` answers `ListHit` with
`marker` set or not, and the frame draws the marker at exactly that column.

## rows

The projection: one pass over the announcement, building nodes keyed by full name,
then a depth-first walk that skips a folded node's children.

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
