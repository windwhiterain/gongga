# tui/markdown.rs

## Nothing is dropped

The rule this file is held to: a reader must be able to reconstruct what the model
wrote from what is on screen. Every marker the source used is printed as itself --
`#`, `-`, `1.`, `>`, ``` fences with their language, `*`, `**`, `~~`, `[text](url)`,
`![alt](url)`, `---`, `[x]`.

The first version of this file did the opposite: it took the emphasis markers (the
parser hands over the text) and dropped link targets, on the argument that this log
gives one style per block and so has nothing to apply emphasis to. That argument is
about *taste*, and the cost was information: a reader could no longer tell that the
model had emphasised a word or where a link pointed. Being unable to reconstruct the
output is the worse failure, so the markers stay.

The second half of that argument is gone too: a row now can be drawn with emphasis in
it, because the colour is read off the marker that was kept. What a marker loses when
it is drawn as itself is only polish; what it loses when it is dropped is meaning and
colour both.

## Structure becomes rows

The genuinely structural events do change the shape: a paragraph is rows, a list item
starts a row, a quote marks the rows it covers, a code block keeps its own rows
(inside a fence, indentation is the document, so it is not re-wrapped or trimmed).
Those are transformations with an unambiguous result, which is what makes them
different from "drop the asterisks".

## A soft break is a space

A newline inside a paragraph is a space in CommonMark, and the fold wraps at the width
the window actually has. Printing the source newlines would fight that wrap and
produce ragged rows. A hard break (two spaces or a backslash) does end a row, because
that is what it means -- and inside a table cell it is a space, because a cell is one
row.

## Why a parser

`pulldown-cmark`, default features off (`getopts` and `html` belong to the binary it
also ships). The alternative was a line-start classifier, and it is wrong the moment a
document nests: a continuation line of a list item, a lazy quote continuation, a fence
inside a list. Those are the cases a reader notices, and a parser already knows them.

Not `tui-markdown`: it produces a `Text` of styled spans, and taking those would mean
rebuilding the log's row model around spans *and* the log's wrap around them. This file
builds its own runs, and hands back `Vec<Line>` -- the log stores rows, and a row is a
list of runs.

## The ink comes from the parse

`rows` is one pass over the events, and the ink is decided where the event is: an
`Event::Text` inside `Tag::Emphasis` is italic because the parse says so, not because
the word happens to sit between two asterisks. The marks are a stack (`Mark`), so
nesting composes -- `**a *b* c**` is bold, then bold-and-italic, then bold again --
and what a mark gives its words is `base.patch(mark.style)`, the block's ink with the
marks over it.

The markers are still printed: the `*`, the `**`, the backtick, the `](url)`. They are
drawn in `marker_style` (the quiet gray), so the words are the loud thing and the source
is still readable. `open_mark_text` is the one place a mark is opened, which is why the
closing text is remembered with it -- a link closes with `](target)`, everything else
with itself.

An earlier version styled the *printed* row instead: it re-read the text a row happened
to contain and coloured `**bold**` between its markers. Two things were wrong with it.
The obvious one is that a run the window wrapped lost its ink on the second row, because
that row no longer held the marker. The subtler one is that the text is not the truth:
`2 * 3 * 4` is arithmetic, and a `#` inside a code block is a comment in the code, and
both were read as markdown by a rule that only saw characters. The parse knows, so the
parse decides.

## The colour and the break are not related

Because the run is decided before the width is known, a break can fall anywhere inside
it: `docs/notes/tui-fold.md` splits the runs and keeps the ink, so a link reads as one
link on however many rows it takes. That is the whole reason the layout produces runs
rather than rows of text.

The one thing that stays per row is indentation: a continuation row of a paragraph
starts at the block's left edge, not under the text it continues.

## The document, and who holds it

Markdown is not append-safe: one more `**` changes a row *above* the one arriving. So
an answer is not appended to; its block holds the *document*, and its rows are what
that document says at the width the block is drawn at.

The document lives in the block (`Entry::source`), not in a front-end accumulator,
because the width is the block's. It used to live in `Tui` as an `Answer` with a
`Drawn` decision -- `Nothing`/`Open`/`Rewrite` -- and that arrangement could not give
a table its columns: the front-end lays out before it knows how wide the pane is, and
a resize afterwards would have no document left to lay out again. Moving it into the
log also deleted the special case in `Tui::stream`: a delta is a delta for every kind,
and `Entry::append` gives an answer's document one more piece and re-lays it out.

A sealed block stops taking deltas, so two turns on one key are two blocks: the first
turn's block stays where it is, and `Log::stream` opens a second rather than growing
the sealed one.

The cost is one layout per delta, and one per block per resize. A few kilobytes
through the parser is microseconds against a frame that is redrawn on the same delta
anyway, and `Log::lay_out` only lays a window out when that window's width actually
moved -- so the parse is paid per delta of the block that streams, never per pane and
never per frame.

## Tables

A table is the one block whose rows depend on each other and on the width, so the
layout width has to be real. `Tag::Table` carries the alignment list, the header is a
`TableHead` of cells, and the body is `TableRow`s; cells are collected in the same row
buffer everything else is drawn in, and taken out at `End(TableCell)` instead of being
kept as a row.

A cell is a `Line` of runs, not a string, because the ink inside a cell is the same
question as the ink outside it: `**bold**` in a cell is bold, and a cell that wraps
keeps its runs. `table::rows` answers with the grid; nothing of the table is drawn
here -- see `docs/notes/tui-table.md` for the columns.

A table inside a quote keeps the quote's marks on every row, and pays for them out of
the width the table may use: the marks are chrome the source wrote, not part of the
grid.

## Raw HTML

A document's HTML block has no rendering here, so it is printed as itself, one row per
source line. The alternative -- dropping it, which is what the event loop did before
it had an arm for it -- is the one thing this file may not do.

## Unfinished markup is literal

While an answer streams, `**bol` renders as `**bol` -- an unterminated marker is text,
not a rule -- and it stays `**bold**` once closed. Nothing is hidden at any point of
the stream, which is what lets a reader watch a document being written.
