# tui/fold.rs

## Why this is not `Log`'s wrapper

`Log` wraps plain text: bytes in, row offsets out, and an append continues the wrap
state from one column count. That is the right machine for a shell's output, and it
is the wrong one for an answer, because a row of an answer is not a string -- it is a
list of runs, and a break can fall inside a run.

Keeping one wrapper for both would mean the log knowing about styles, or the markdown
renderer knowing about row indexes. So the document folds its own rows, and the log
stores the result: `Vec<Line>`, one per drawn row, counted and drawn as they are.

## Nothing here may panic

This runs inside the window, on every event, so a panic is not a bug report -- it is a
dead session. The first version had two `expect`s and one of them was reachable: a
streamed chunk that was only spaces, arriving at the end of a full row, trimmed to
nothing and then asked for its first character. That is ordinary traffic for an answer
(a delta is a few characters, and a space is one of them).

So every step of `rows` answers with something it can carry on from. A span trimmed to
nothing ends the loop for that span. `first` answers `Option`, and the caller treats
`None` as "nothing left to draw". The `last_mut` that pushes a row is a `match`, not an
`expect`, because the row it names is a value this function just pushed. `push` ignores
an empty run instead of adding a span that draws nothing.

The rule that follows from this: in draw-path code, an index is chosen with `get` or
proved by construction, and a helper that can have no answer says so in its type.

The shape that cannot fail is the fix; finding the one reachable `expect` was only how
it started. `Tui::draw` also runs the whole frame under a net (`guarded`), which is a
last resort for one that has not been found yet -- see `docs/notes/tui-lib.md`.

## rows

One span at a time, filling the current row until the next character does not fit.
Nothing is copied that does not change: a span that fits whole is pushed whole, and
only a span a break lands inside is split -- which is exactly the case that used to be
wrong, when the row's text was styled after the fact and the second half had lost the
marker that said what ink it was in.

`Room::done` is what makes a word break stick. Without it, backing up to a space leaves
the row with columns to spare, and the next word simply continues on the same row --
`the f` / `irst`. The flag says "this row ended at a word, even though there is room".

A continuation row does not start with the space that broke it: the space goes with
the row it ended, so a fold never produces a leading blank. That is also the only
character a fold can drop -- a space at a break, which no reader can distinguish from
the break itself. Every other character is drawn.

## room

Greedy, and word-aware when a word is in reach: fill to the limit, then -- if the cut
would land inside a word -- back up to the last space that fits. A word longer than
the row has no space to back up to and is cut, because it has to go somewhere.

The character at the boundary is measured with `UnicodeWidthChar`: a CJK character is
two columns, and a row that measured it as one would draw one column too many and
push the table it sits in out of line.

## first

The one case greedy wrapping cannot serve: a character wider than the whole row (a CJK
character in a one-column column). It is drawn anyway -- at the cost of the row
overflowing by a column -- because the alternative is a loop that pushes a new row
forever, or dropping the character.
