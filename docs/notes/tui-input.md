# tui/input.rs

## input.rs

The input line: editing primitives plus the completion popup.

Nothing here knows what a command is. The caller hands this module an arbitrary
list of candidates for the token under the cursor and it renders the popup;
deciding *what* completes is the command layer's job, and both the candidates and
the token split come from there.

What stays here is the part that is genuinely the editor's: what a keystroke does
to the text, and how a chosen candidate is spliced back in.

## InputLine

A text editor with a byte cursor.

Multi-line, because a message to the agent is prose: a paragraph break is part of
what is being said, not a second message. The breaks live in the text itself
(`'\n'`), so everything that already treats the text as an opaque string --
history, submission, the wire -- keeps working unchanged.

## InputLine::line_bounds

Byte range of the logical line the cursor is on, newline excluded.

Logical lines, not the rows the terminal wrapped them into: a break the operator
typed is a place they can navigate to, and a wrap is only where the glass ended.

## InputLine::to_document_end

A restored draft puts the cursor at the very end of the text rather than of the
line it happens to end on: `restore_input` is the only caller, and the saved text
may hold several lines. History recall does not come through here -- it replaces
the text and lets the cursor land where the replacement leaves it.

## InputLine::up

Moves to the line above, keeping the column. False when there is no line above,
which is what lets the caller fall back to history.

## boundary

`at`, moved back to the nearest char boundary.

Vertical moves keep a byte column, and a column that lands inside a wide character
has to round down rather than split it: slicing a `String` at a non-boundary
panics.

## a_newline_starts_a_line_the_cursor_can_leave

A message can have paragraphs, so the text has to survive a newline and the
vertical moves have to walk them -- otherwise the second paragraph is reachable
only by arrowing through the first one character at a time.

Home and End are line-scoped now, not text-scoped; the document end is still
reachable, which is what a restored line and a recalled history entry need.

## vertical_moves_round_down_to_a_char_boundary

A column lands on a byte, and a byte can be inside a wide character.

Column 2 on the second line; `中` is three bytes, so the same column on the first
line is inside it and has to round back to its start.
