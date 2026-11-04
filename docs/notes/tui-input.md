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

## InputLine::selection

The bytes a selection covers, if there is one.

A selection is an *anchor* and the cursor: the anchor is the end that stays put,
the cursor is the end that moves. The two are the same when there is no
selection, which is why the anchor is `Option` and not a pair.

The anchor is the mouse's business -- a press sets it, a drag moves the cursor
around it -- and the keyboard's business is only to end it: every key that moves
the caret on its own clears the anchor, so a selection and a moving caret are
never two different things at once.

## InputLine::collapse

Ends a selection by putting the caret at one of its ends.

The end the key names: the start for a move to the left, the end for a move to
the right. One press therefore reaches the edge of what was selected instead of
walking off it -- the same thing every editor does, and the reason a selection
that is 500 characters long can be thrown away with one key.

Vertical moves collapse to the *start* and then move, because the column a
vertical move keeps is measured from the start of the line.

## InputLine::delete_selection

Deletes the selected bytes, if there are any.

Every editing key asks this first: an insertion replaces the selection, a
backspace deletes it, a delete deletes it, and `Alt+K` kills it. The caret ends
where the selection began, so the text is left where the operator pointed at it.

## The mouse in the input box

A click puts the caret where the pointer was; a drag selects from there.

The pointer's column is a *display* column, so it has to be turned back into a
byte offset: the prefix takes the first two columns of the first row, the rest
of the row is the text, and a character that is two columns wide is entered at
its own start. The offset is always a character boundary -- the text is cut at
it, and a cut inside a character panics.

The click lands on the character *before* the column it was over, which is what
a terminal can say: it reports the cell, not the half of the cell.

A drag that leaves the box is pulled back into it, so the selection ends at the
text's edge rather than at nothing.

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

Moves to the line above, keeping the column. False when there is no line above:
that is the top of a one-line prompt, and nothing else answers there now -- the
history is `Shift+Up`'s, so a bare arrow that finds no line to move does nothing.

## boundary

`at`, moved back to the nearest char boundary.

Vertical moves keep a byte column, and a column that lands inside a wide character
has to round down rather than split it: slicing a `String` at a non-boundary
panics.
