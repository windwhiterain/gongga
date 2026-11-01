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

What a marker loses when it is drawn as itself is only polish. What it loses when it
is dropped is meaning.

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
rebuilding the log's row model around spans. Here the parse is used for structure and
the spans are not needed at all.

## Answer::feed and the rewrite

Markdown is not append-safe: one more `**` changes a row *above* the one arriving. So
an answer is accumulated, rendered again whole, and its block rewritten
(`Log::rewrite`) -- unlike every other block, which is appended to.

`Drawn` is what that decision returns (`Nothing`, `Open`, `Rewrite`), so the rule is
testable without a terminal, which `Tui::stream` is not.

`opened` lives in the accumulator because a sealed block stays in the log under its
key, so "a block exists with this key" cannot answer "have I opened mine". Two turns
on one key are two blocks, and the second must not rewrite the first.

The cost is one re-render per delta. A few kilobytes through the parser is
microseconds against a frame that is redrawn on the same delta anyway.

## Unfinished markup is literal

While an answer streams, `**bol` renders as `**bol` -- an unterminated marker is text,
not a rule -- and it stays `**bold**` once closed. Nothing is hidden at any point of
the stream, which is what lets a reader watch a document being written.
