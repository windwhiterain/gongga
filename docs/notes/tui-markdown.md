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

A newline inside a paragraph is a space in CommonMark, and the log wraps at the width
the window actually has. Printing the source newlines would fight that wrap and
produce ragged rows. A hard break (two spaces or a backslash) does end a row, because
that is what it means.

## Why a parser

`pulldown-cmark`, default features off (`getopts` and `html` belong to the binary it
also ships). The alternative was a line-start classifier, and it is wrong the moment a
document nests: a continuation line of a list item, a lazy quote continuation, a fence
inside a list. Those are the cases a reader notices, and a parser already knows them.

Not `tui-markdown`: it produces a `Text` of styled spans, and taking those would mean
rebuilding the log's row model around spans. Here the parse is used for structure only,
and the spans are built at draw time from the markers a row prints (`colored_row`), so
the log keeps handing over text.

## Colour comes from the markers

`colored_row` is the other half of the file, and it is the reason the markers had to
stay: a frame draws rows, so what it can colour by is what a row prints. A marker is
read where it sits -- `# ` opens a heading, `- ` / `N. ` a list item, `> ` a quote,
`**` a strong run, `*` emphasis, `~~` a strike, a backtick a code span, `[text](url)` a
link, a fence and `---` their own rows -- and the words it covers take the ink that says
what they are. The marker itself is drawn in the quiet gray, so the words are the loud
thing and the source is still readable.

What it reads is this file's spelling, not the writer's: `render` writes `*`/`**` for
emphasis however the document spelled it, `- ` for every bullet, `+ ` / `* ` where the
writer used them, and `N. ` for every numbered item. A document whose markers did not
come through `render` has no colour, which is exactly right: nothing in the log is styled
by a rule it did not print.

Two rules keep the colour honest. A pair needs both markers on the row and words between
them with no space at either end: `2 * 3 * 4` is arithmetic, and a marker read there
would colour a word the writer never marked. And only markers colour anything -- a
paragraph's own words keep the window's foreground, which is what a reader is reading.

A row whose colour its own text cannot say carries it: `Ink`. That is the code block's
body, a table's rows, and the fences around a block -- the four cases where the marker
that would say what the row is (a fence, a pipe, none at all) is either absent or
ambiguous. Everything else is read off the text, because everything else prints a
marker and a row of this log is its text.

The split is not cosmetic: a `#` inside a code block is a comment in the code, and a
`|` inside a cell is not a column. Reading ink off the text would get both wrong, so
the block that knows -- the layout, which saw the fence open -- is the one that says.

Colour is per row and markers are not: a link the log wrapped across two rows draws its
second half as text, because that row no longer holds the `[`. The text is right and the
ink is one row behind; carrying the ink across the wrap would mean styling rows by
something other than what they print, which is the rule that makes the rest of this
sound.

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
anyway, and `Log::reflow` only walks the whole log when the width actually moved.

## Tables

A table is the one block whose rows depend on each other and on the width, so the
layout width has to be real. `Tag::Table` carries the alignment list, the header is a
`TableHead` of cells, and the body is `TableRow`s; cells are collected in the same row
buffer everything else is drawn in, and taken out at `End(TableCell)` instead of being
kept as a row.

`table::rows` then answers with the grid, and every one of its rows becomes a `Piece`
with `Ink::Table`, except the rule, which is `Ink::TableRule`. Nothing of the table is
drawn here -- see `docs/notes/tui-table.md` for the columns.

## Ink

Five answers, and each one is a statement about what a row *is*: `Prose` (every marker
in it is markdown), `Code` (a code block's body), `Fence` (the ``` around it), `Table`
(a row of the grid), `TableRule` (the rule between header and body).

There is no `Heading` or `ListItem` because those print their marker and the renderer
reads it: a second source for the same fact is a source that can disagree.

## Raw HTML

A document's HTML block has no rendering here, so it is printed as itself, one row per
source line. The alternative -- dropping it, which is what the event loop did before
it had an arm for it -- is the one thing this file may not do.

## Unfinished markup is literal

While an answer streams, `**bol` renders as `**bol` -- an unterminated marker is text,
not a rule -- and it stays `**bold**` once closed. Nothing is hidden at any point of
the stream, which is what lets a reader watch a document being written.
