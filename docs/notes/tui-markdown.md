# tui/markdown.rs

## Why a parser, and which one

SPEC's `output (colored markdown)` asks for the *shape* of an answer to be visible: a
heading, a list, a quote, a fenced block. `pulldown-cmark` is the parser, with its
default features off -- `getopts` and `html` are for the command-line binary it also
ships, and this crate wants neither.

The alternative was a line-start classifier of about thirty lines, and it is wrong the
moment a document nests: a continuation line of a list item, a lazy quote
continuation, a fence inside a list. Those are the cases a reader notices, and they
are exactly what a parser already knows.

It is *not* the ratatui markdown crate. `tui-markdown` produces a `Text` of styled
spans, and this log gives one look to a whole block -- taking its spans would mean
rebuilding the log's row model around them. What is wanted here is the structure, so
the parser is used directly and the spans never come into it.

## Structure only, and what that means for markers

What is kept is structure: heading level as `#`s, list items as `•`/`1.`, quote rows
behind `│`, a code block indented two columns, task markers as `[x]`/`[ ]`, inline
code keeping its backticks (a reader needs to tell a command from prose about one).

What is dropped is the marking: `**bold**` arrives from the parser as its text, so
emphasis disappears rather than being drawn as asterisks. That is the honest outcome
of "structure only" -- this log has one style per block, so there is nothing to apply
emphasis *to*. Drawing the markers instead would be showing the source, not the
document.

Links keep their text and lose their target: a terminal cannot follow a URL, and the
target would double the width of the row it sits on. Images contribute nothing.

## A soft break is a space

A newline in the source is not a row break: the window is narrower than the document
was written, and the log already wraps. Keeping the source's newlines would produce
ragged rows with a ragged right edge, and one short row per sentence.

## Answer::feed and the rewrite

Markdown is not append-safe. One more `**` changes a row *above* the one arriving, so
an answer cannot be streamed into the log the way every other block is: the deltas are
accumulated, the whole answer is rendered again, and the block's text is replaced
(`Log::rewrite`).

`Drawn` is what that decision returns -- nothing yet, open, or rewrite -- so the rule
is testable without a terminal, which `Tui::stream` is not.

`opened` is part of the accumulator for one reason: a sealed block stays in the log
under its key, so "a block exists with this key" cannot answer "have I opened mine".
Two turns on one key are two blocks, and the second one must not rewrite the first.

The cost is a re-render per delta. A few kilobytes through `pulldown-cmark` is
microseconds against a frame that redraws on the same delta anyway, and the alternative
-- incremental block rendering -- is a second implementation of what the parser
already does.

## Unfinished markup is literal

While an answer streams, `**bol` renders as `**bol`: an unterminated marker is text,
not a rule, and the parser says so. It becomes bold -- that is, the markers disappear
-- on the delta that closes it. A reader watching the stream sees the markers go, which
is the truth about a document still being written.
