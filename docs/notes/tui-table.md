# tui/table.rs

## Why a module of its own

A markdown table is the one block whose rows depend on each other: a column is as
wide as its widest cell, so a row cannot be drawn before the whole table has been
read, and the width the table gets is the frame's, not the writer's. `markdown.rs`
turns events into rows one at a time; a grid is the case that cannot. Splitting it
out keeps the event machine in `markdown.rs` and the arithmetic here, where it can be
tested without a parser or a terminal.

## Align

GFM spells alignment with colons in the rule row: `:---` left, `:---:` center, `---:`
right, `---` neither. The writer's choice is kept and printed back, because the rule
row is part of the table the model wrote -- a reader must be able to see that a
column was aligned, not just that it happens to look aligned.

`Align::None` is drawn left like `Left`: no marker is not a fifth alignment, it is a
column the writer left to the reader, and left is how a table reads.

## rows

The header first, then its rule, then the body, with one rule on every boundary
between two logical rows. Every row is the same display width, which is what makes
the table a table: the frame draws rows, so alignment only exists if the rows are
padded.

The body's rules are what a table needs to stay readable: a cell whose text wrapped is
several drawn rows, so a wrapped `that wraps` and the next row's `bob` sat on adjacent
lines with nothing between them, and the eye read one record as five. A rule between
two logical rows is what says where a record ends.

Nothing follows the last row: its own right border closes it, and a rule there would
be a border the source never wrote. A header with no body keeps its rule, because
that rule is the header's, not a boundary's.

A cell is a `Line` of runs, not a string: what ink a word was written in is the
markdown renderer's answer, and it has to survive the column. `fold::rows` does the
wrapping inside the column -- the log's wrapper would put a second row's worth of a
cell where the next column's space is.

`rule` answers with a `Line` whose runs are all the marker ink. It is the table's
chrome rather than one of its cells, and a rule recognised by its characters later
would be a rule guessed at.

## fit

The natural width of a column is its widest cell; a table that fits is drawn at its
natural width, because a squeezed column wraps text that had no reason to wrap.

When it does not fit, the columns that give up room are the widest ones: the
algorithm sorts by natural width and hands each column the smaller of "what it needs"
and "an even share of what is left". Widest last, so the narrow columns keep their
natural width and only the wide ones wrap -- a `#` column and a prose column do not
give up the same amount.

`MIN` is three, which is what the rule row needs to be a rule: `---`. A column
narrower than that could not be marked as a column at all. When the frame cannot hold
one `MIN` per column plus the borders, the table overflows its width and the log wraps
those rows -- a wrapped table is ugly but complete, and dropping columns to fit would
lose what the model said.

## compose

One logical row can be several drawn rows: a cell that wraps needs its neighbours'
rows padded beside it. The height is the tallest cell's, and every shorter cell is
padded with blanks -- unchanged text, just air where the column has none.

The segment between two borders is `width + 2` columns, and it is the same in every
row: one column of air, the alignment's padding, the cell, the rest of the padding,
one column of air. The rule row spends the same `width + 2` on its dashes and colons,
so a table's rule is exactly as wide as the rows around it -- which is what the first
version of this file got wrong, by one column per column.

A word wider than its column is cut at the column's edge rather than being allowed to
overflow: the alternative is a row that pushes every column after it out of line.

## pad

`UnicodeWidthStr`, not `len`: a cell of Chinese text is twice as wide as its byte
count, and padding by bytes would leave the borders ragged exactly where the model
writes in a language this window does not use Latin letters for.

## rule

One function draws every boundary, and the alignment markers are what its caller says
it states: the header's rule is given the columns' alignments, a body boundary is
given none. That is GFM's own accounting -- `|:--|--:|` is written once, under the
header, and a table does not restate its alignment under every row. A body rule with
colons in it would also read as a claim about that row.

Dashes fill the column, and the colons are taken out of the dash count rather than
added to the width: `:---:` is the same width as `---`. Otherwise the rule row would
be wider than the rows around it and every table would be one column off from its own
header.

The rule is `|`, dashes and `|`, never box-drawing: `markdown.rs` prints the source's
markers in the quiet ink, so a table is drawn in the characters it was written in.
