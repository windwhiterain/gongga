# tui/lib.rs

## lib.rs

`gongga-tui`: the terminal front-end.

This crate is deliberately ignorant. It renders lines, keeps an input line, and
shows a completion popup whose contents somebody else supplies. It has no
dependency on the wire protocol and no idea what a command is: the command
layer (`gongga-cli`) owns the catalog, the wire, and the meaning of every line,
and hands both completion candidates and typed lines over.

That is why the dependency runs this way -- `tui` depends on `cli`, never the
reverse. A command layer that knew about terminals could not be used from a
script; a front-end that knew about commands would have to change every time one
is added.

The `gongga-tui` binary lives here because the window is the only program that
needs both halves, and linking them is what a binary is for. The command line is
`gongga-cli`, in the crate this one depends on.

## cli re-exports

The vocabulary belongs to the command layer: this crate paints whatever it is
told to paint. Re-exported so `crate::LineKind` and friends keep reading the
same from inside this crate -- the renderer and the log never have to know where
the words came from.

## Tui::new — the clear

`EnterAlternateScreen` switches to the alternate screen; it does not empty it. On
a terminal that keeps those contents, a process killed rather than quitted leaves
its last frame there, and the first `draw` will not paint over it: ratatui diffs
against its own empty buffer, so a cell holding a space is never written.

Hence the explicit `terminal.clear()` before the first frame. It is one sequence
on every start, and the only thing that makes a start look like a start.

## Tui::draw — the caret, and why the frame is applied by hand

`Terminal::draw` flushes the frame and then, for the caret, always calls
`show_cursor` + `set_cursor_position`, even when the caret has not moved. Every
repaint therefore repositions the console caret -- and on Windows that is exactly
the moment an IME re-places its composition and candidate window. An answer that
streams repaints several times a second, so a caret and a candidate list that were
sitting still flicker for as long as the answer arrives.

So the two halves are done here: `autoresize` + `get_frame` + `render::draw` +
`flush` + `swap_buffers` write the frame, and the caret is touched only when it
actually moved. A frame that only appended to the log now costs no cursor sequence
at all, and a frame that followed typing costs one.

The input area the caret belongs in is the one `render::draw` returns, not a second
computation of it: `autoresize` can change the size between the two, and a mismatch
is a `MoveTo` and a `Clear` on the wrong row -- which is what an earlier version of
this did, blanking the first cells of the help row.

After a change to the input, the box is also erased from where each row's text ends
to the right edge (`render::input_ends`). An IME draws its composition into the
console on that row, the diff never sees that ink, and so a composition longer than
the text committed from it leaves its tail behind with nothing to overwrite it. What
is erased is what the frame drew as blank, so the erase can only take away ink the
console put there. Erasing and rewriting the whole row (a first attempt) corrupted
the row below it in a live run.

## scroll_by and scroll_to_bottom

SPEC's page keys act on the *sticky session*: PageUp/PageDown scroll it, Shift+Down
takes it back to its newest line (`Panes::pin`). The keyboard therefore does not act
on whatever was last clicked; the click sets the sticky, and the keys follow it.

With no sticky session there is nothing to scroll, and a page is a no-op rather than
a guess at which pane the operator meant.

## Screen::session_at_row

What a pointer landed on, as a `Hit`: the session it names and which part it hit --
the list, a header, a window's events, or the way back to a window's newest row. They
have different behaviour, so the part is part of the answer rather than something the
caller re-derives.

The order is the drawing order: the list, then the go-to-bottom hint, then the
headers, then the events. The hint is drawn over the last row of the window it belongs
to, so asking the window first is how a click on the button became a click on the
session -- which asked the command layer for a `set_sticky` nobody pressed.

## Screen::item_at

The item under a cell of a window, as the key a press toggles. The slot gives the
row inside the window, and the log answers which block that row belongs to -- the
same rows the frame drew, fold included, so a press on a summary line opens the item
it summarises.

## Screen::cell_at, Screen::body_at, Screen::scroll_at

One cell of a window, in the terms the log answers in: the window's log width (one
column short of the drawn one, as the renderer measures), the rows it shows, its
scroll, and the row inside it. A press and a wheel therefore ask about the same
arithmetic the frame was drawn with, and cannot land on a row it drew something else
on.

`item_at` answers the first question -- which block a press names, fold included.
`body_at` answers the second: the same walk, minus the box's two rules. A rule is the
box's edge, not content, so a wheel over it belongs to the pane. That is the border an
operator uses to reach the window while a box is open over most of it.

`scroll_at` gives one notch to one thing. The box of an opened item takes it while it
can move in that direction; the pane takes it otherwise. Which of the two is decided
nowhere else, so a box is never a cell where the wheel does nothing, and the same cell
behaves the same way whatever `block_size` is. See `Log::scroll_block`.

## handle_mouse

The wheel scrolls what it is over, and retargets nothing: over a window it scrolls
that session's rows, and over the session list it scrolls the list, which has a scroll
of its own (SPEC gives it one). The keys keep acting on the sticky session whatever
the pointer did.

A click is a different thing, and SPEC gives it two jobs. On a name it toggles that
session's window: the line is `/set_render_session --session <name> --show <!shown>`,
with "shown" read from what the window draws (`Kind::Shown`). On a header it sets the
sticky session: `/set_sticky --name session --value <name>`. Both are handed to the
loop as a submitted line, so the rule stays in the command layer and the window keeps
one path for everything the operator asks for. The leading `/` is what makes it a
command: a line without one is prose, and prose goes to the agent.

Hiding a session that is shown therefore sends one line, not three. The sticky
follow-on -- showing makes it sticky, hiding the sticky one clears it -- is
`Client::follow_shown`, on the other side of the wire, so a typed
`/set_render_session` behaves the same as the click.

The go-to-bottom hint is not a session: it pins that window to its newest row
(`Panes::pin`) and asks the command layer for nothing. A click in the events area
does nothing at all -- the header is what a session is clicked by.

## Screen::sticky_session

Filled from `Sink::sticky_session` on every refresh, so the keys, the wheel's target
and the frame around that window all read one value: the one the command layer has in
`Sticky`. The list does not read it -- its brightness answers "is there a window".

## UiEvent

There is deliberately no "cancel" here. Stopping work is a command like any
other (`interrupt_session`, `cancel`), so it reaches the server by the same path
as everything else and can be typed by a script that has no keyboard. The only
thing left to the keyboard is leaving.

A `Submit` may contain newlines: a message with paragraphs is still one message.

## Screen

Everything the renderer reads. Kept separate so drawing never borrows the
terminal while the caller is mutating state.

## Screen::panes

The scrollback of every window the command layer named, each with its own scroll.
See `docs/notes/tui-panes.md`. The crate stays ignorant of what a window *means*: it
holds the names it was handed. A `Log` is still its own type because a frame must not
be allowed to walk it -- that is the difference between a usable window and a frozen
one, and it is now a rule per pane rather than for the whole area.

## Screen::note

What the harness says about itself -- "nothing is listening at 127.0.0.1:9700 —
starting one", "connection refused: bad key", "copied 42 characters", a server log
line -- goes on the status line, with the kind it was said as, so the header bar can
paint a failure as one.

Nowhere else, and the two alternatives are wrong in the same way:

- the command row is SPEC's `[{cmd, cmd_result}]` and keeps only the newest entry, so
  a note there joins a ticker of things the operator typed -- that is how "starting
  one" came to sit where an input line is expected;
- a session's lines are *that session's events*. "nothing is listening" is not
  something a session said, and putting it there makes a conversation's log report
  on the transport.

A note replaces the previous one, so the line always reads as the newest thing this
window knows, and a refresh from the server puts the facts back.

`connected to the server` is deliberately not one of these: the status line going
from `offline — reconnecting (attempt 1)` to `3 session(s)` is that
sentence said in facts. A build mismatch is said *after* the refresh, so the facts
do not wipe it in the same breath.

## Screen::sticky_session decides the keyboard

There is no `focus`: no "which pane did the pointer last touch", no second name that
can drift from the one the keys obey. The sticky session decides everything the
keyboard does -- which window a page scrolls, whose window is framed, which column
takes the width an even share leaves over -- and a click is what changes it, by asking
the command layer for the command that sets it.

The wheel retargets nothing, so a glance at an older line does not move the next page
key. A click sets the sticky, and that is the only way the keys' target moves.

What the wheel does write is the scroll of what it is over, and the session list is
one of those views: SPEC gives the list a `(vertical scroll)`, so it holds an offset
of its own (`render::list_window`) instead of being derived from the sticky session.
A wheel over the list moves the list and nothing else, so the name the keys send to
does not depend on which row of names the operator happened to be reading.

## Screen::slots, Screen::visible_rows, Screen::page_height

The layout questions a frame, a copy and a key all ask, answered by the same
`render::areas` and `render::pane_slots` the renderer uses -- so what a copy takes and
what a page moves are what was on the screen. They live on `Screen` rather than on
`Tui` because they need no terminal, which is also what makes them testable.

## Screen::scroll_by

Positive scrolls into the past, negative back towards the newest line, and both stop
at the ends. It moves the sticky session's view only: an operator reading an older
answer in one session should not lose their place in another that is still streaming.
The upper bound is the renderer's, since only it knows how many rows the lines wrap
into and how tall this pane's window is.

## Tui::dirty

Whether anything on screen changed since the last frame.

A redraw is a terminal write, and while a response streams the loop gets an event
for every chunk. Painting a frame nothing has changed is pure cost, so the frame
is skipped instead -- the flag is what keeps that from freezing the display:
every mutation and every key sets it.

The flag starts `true`, because the first frame has to happen even though nothing
has changed.

## Tui::new

Mouse capture is what makes the wheel a scroll key. Without it crossterm never
sees the wheel at all -- the terminal keeps the events for its own scrollback,
which an alternate screen has none of.

## Tui::set_status

`set_status` only asks for a frame when the text actually changed: the caller
recomputes the status from the session list as events arrive, and most of those
recomputations say the same thing as the last one.

The footer is gone (see `docs/notes/cli-lib.md`: SPEC dropped its row, and the
information lives on the canvas), so the status line is the only chrome this window
owns.

## Tui::restore_input

The counterpart of Enter: a line that could not be sent is not swallowed by a log
entry. It goes back where it was typed, because the operator already knows how to
press Enter again and does not want to retype it.

## Tui::stream

`key` identifies the stream (a response part, a tool's stdout) and `done` closes
it, so the next chunk with the same key starts a new line.

## Tui::forget

`Sink::forget`, for a window: the command layer watched the session leave the
announcement and says so, because the name can come back. The pane and its blocks go
with the name; the status line and the sticky session are the command layer's, so
neither is touched here -- a sticky session naming a deleted session scrolls nothing.

## Tui::draw — the frame is under a net

Drawing runs on every event, so a panic inside it is not a bug report: it takes the
terminal, the turn in flight and the operator's place in the log with it. The frame is
therefore painted through `std::panic::catch_unwind` (`guarded`), and a frame that
broke is abandoned rather than thrown: `Terminal::clear` resets the back buffer so the
half-drawn one cannot be diffed against, the status row carries what the payload said,
and the next event paints again.

This is a net, not a fix -- the fix is that the draw path has no reachable panic left
(see `docs/notes/tui-fold.md` and `docs/notes/tui-log.md`). The net exists because the
window has already died several times to one bug, and "abandon this frame" costs an
operator a repaint while "abort" costs them the session.

It is sound because a frame owns nothing: the screen is a value this struct holds, its
rows are recomputed from the log every time, and nothing is left half-committed for the
next frame to trust.

## Tui::draw

Nothing changed, so nothing is written. This is what keeps a stream that arrives
faster than the terminal can paint from spending the whole loop in the renderer.

The renderer writes the scroll bound back into the screen, which is why it takes
it mutably even though it only reads the log.

`dirty` is cleared only after the frame landed: a failed paint must be retried by
the next call rather than counted as done.

## Tui::next_event

Paints a frame before every wait, so lines pushed by the caller show up even with
nobody at the keyboard, and after any event that could have changed what is on
screen.

Only key events used to be looked at, which is why the wheel did nothing: it
arrives as a mouse event and was dropped here. A resize needs no handling of its
own beyond the redraw it asks for -- the layout is worked out from the area every
frame, and the log reflows to the new width when it is asked for rows.

Windows reports a `Release` in addition to every `Press`, and some rebuilds also
emit `Release` on other platforms. Without the filter one keystroke moves the
cursor twice or types twice.

## Tui::actionable

Only presses matter. `Release` is dropped so that a single keystroke is handled
once; `Repeat` (a held key) is kept, because holding Backspace or an arrow is
expected to keep working.

## Tui::shortcut

The keys that are not editing at all: only leaving, which is the one action this
window owns rather than asks the server for.

Pure and separate from `handle_key` because it is the part worth testing without
a terminal: what a key means must not depend on whether a TTY exists.

Alt, not Ctrl: VS Code's terminal (and several others) consume Ctrl+Q, Ctrl+K and
Ctrl+arrows themselves, so a Ctrl binding is one the program never sees. `Ctrl+Q`
used to be the exit here and simply did nothing in that terminal. `Ctrl+X` and
`Ctrl+C` are gone for the same reason.

## Tui::line_break

Shift is what everyone tries; Alt is the spelling that survives terminals which
do not report a modified Enter (they send the same CR for both), because Alt+Enter
arrives as ESC CR and is always distinguishable.

## Tui::handle_key

The other modifier bindings sit on Alt for the same reason as the exit: a
terminal that keeps Ctrl+K for itself is not unusual, and a binding nobody can
press is worse than no binding.

Paging lives on the arrows that already mean "history" and "cursor": Alt+arrows,
because PgUp/PgDn on a laptop hide behind Fn, where some of them are not even
wired to scroll.

Ctrl+<letter> would otherwise insert its letter: a terminal that does deliver
Ctrl+K here (some do) must not type a `k`.

Nothing in this branch knows what an arrow means: SPEC's three bindings on ↑/↓
are told apart in `arrow_key` and performed in `arrow`, which is what a test can
reach without a terminal.

A page is the log window, not a fixed ten rows: on a short terminal ten rows
scrolls past what is visible and the jump looks arbitrary.

## Arrow

SPEC's bindings on the vertical arrows, told apart before any state is read:
`Shift+Up` the last command line, `Shift+Down` the newest row of the sticky
session, a bare `Up`/`Down` the line above or below the caret.

# Invariant

The recall is Shift's, and a bare arrow is the prompt's, so the two are never the
same keystroke. Left to the shell convention the history sat on the bare `Up` --
which is also the key every editor uses for the line above, and on a one-line
prompt it is the one thing an operator presses by reflex. The result was that a
message already sent came back into the prompt when nothing was being recalled.
Splitting the two keys is what makes "the line above" mean the line above.

## Tui::arrow_key

The key table alone: which code with which modifiers asks for which of the three.
Alt+arrows never reach it -- they page, above the Alt branch -- so `Line` is only
ever a bare arrow.

## Tui::arrow

Performs one of them. The popup answers before the text does: it is what the
operator is looking at, and moving the line behind it would move the text they
are choosing for.

## Shift+Delete

SPEC gives Shift+Delete the sticky session, and not the editor's Delete or
Backspace: those two erase a character, so the same key did two unrelated things
depending on whether the line happened to be empty. The window asks for the
deletion the way a click does -- it submits `/del_session --session <name>` --
and the command layer owns the rule.

The modifier is what makes the binding a delete and not an edit, so a bare
Delete still erases a character, in the prompt as everywhere else: this key is
the one action that is never decided by where the caret happens to be. It needs
an empty prompt anyway, because the prompt is the editor's -- and it needs a
sticky session, because with none set there is nothing to delete.

A terminal that does not report Shift for Delete sends the plain one, which the
editor takes: the binding then simply never fires. That is the safe direction --
a session deleted by a key that was not pressed is not recoverable.

The binding deletes the session the sticky names; the sticky setting itself is
the command layer's, and clearing it is a `del_sticky` nobody pressed.

## clipboard_paste and paste_from_clipboard

SPEC makes `Alt+V` the paste. Where the terminal pastes by itself no key
arrives; where one does, `paste_from_clipboard` reads the clipboard and hands the
text to `Tui::paste`, so a paste from the key and a paste from the terminal take
the same road.

Alt is the binding rather than a tolerated extra, because the Ctrl spellings do
not survive a terminal that wants them for itself: VS Code binds `Ctrl+V` to its
own paste, and never writes `Ctrl+Alt+V` into the pty at all. `Alt+V` reaches
this window as `KeyCode::Char('v')` with `ALT`; `Ctrl+V` is still accepted, for a
terminal that forwards `^V`.

# Invariant

The key is read above the Alt branch and above the Ctrl swallow, or it would
look like a dead key: the Alt branch returns `None` for every letter it does
not know, and the swallow below it drops the rest. What the read says when it
fails is the status line's business, never the prompt's — and an empty
clipboard says nothing at all, because "nothing to paste" is not news.

## Tui::paste

A bracketed paste arrives as one event carrying one string. It is inserted with
`insert_str`, never replayed as keystrokes.

# Invariant

A newline inside a paste must not press Enter. Before bracketed paste was
enabled, the terminal sent the paste as a burst of key events, so pasting a
programme submitted its first line and left the rest in the box — the paste
looked like it "lost" everything after the first break.

The popup is closed on paste: it was about the token before the paste, not about
what the pasted text means.

## Tui::console

Alt+Y and Alt+M act on the terminal, not on the conversation, so they are not
`shortcut` (which returns a `UiEvent`). Copy reports what it did in the log;
mouse reports which mode it just entered, because the operator cannot otherwise
tell whether a wheel scroll or a terminal selection will happen next.

## Tui::copy_visible

Copies the rows currently drawn, in the order the frame drew them, by the same
layout (`Screen::visible_rows`) -- one column short, pane by pane, command line
last. A copy that took a different width would put back text the screen had wrapped
differently, and a copy that took one pane would drop the answer the operator was
looking at in another.

## Tui::toggle_mouse

Holds no opinion about which mode is better, and flips a flag only after the
terminal accepted the change: a flag out of step with reality makes the next
toggle lie.

# Invariant

Releasing mouse capture is what makes native copy possible at all. While the app
holds the mouse, the terminal never sees a drag and cannot select — which is why
"the TUI cannot copy" was true before this key existed.

## Tui::handle_mouse

The mouse is the pointer the split needs: a notch is three rows, Shift slows it to
one, and the view under it -- hit through the same rectangle the renderer filled --
moves. Over an opened item's body the notch is the box's while the box can still move
that way, and the window's afterwards; over the box's rules, or anywhere else in the
window, it is the window's. Over the session list it scrolls the list, which has a
scroll of its own (SPEC gives it one) because a list is not a session. Neither leaves
state behind: the keys still act on the sticky session.

A press is different, and it is the only reason a click does anything at all: on a
name it toggles the session's window, on a header it sets the sticky session, each as
the command that does so. No click is an action on the conversation, because
everything that changes state is a command.

## Tui::wheel_rows

What a wheel event meant, in rows: positive is into the past. `None` for
everything else the mouse can report, none of which is an action here.

## Tui::page_height

The input's height depends on what has been typed into it, so a pane's does too, and
with a split the page is one pane's rows rather than the area's: a page of scrolling
should land where the operator is looking, not a screenful of the whole area.

## Tui::newline

The popup was about the token before the break; it says nothing about the one
after it.

## Tui::apply_completion

`--session` is not the end of the thought: its value is. Asking again right after
applying is what makes Tab twice fill both, instead of Tab, and then a letter
before the list comes back.

It costs nothing when the candidate ended the line: an empty answer simply leaves
the popup shut.

## Tui::history_prev

`Shift+Up`, one line back per press, stopping at the oldest line: the prompt's
whole history is one key and one direction. Nothing walks forward again --
`Shift+Down` is SPEC's "back to the newest row" -- and Enter clears the position,
so the next recall starts from the newest line.

# Invariant

Never a bare `Up`. The arrow belongs to the prompt's own lines, and a one-line
prompt has no line above, so that key does nothing there rather than quietly
replacing what was typed with something sent earlier.

## Tui::restore

Leaving the capture on would keep the wheel (and text selection) away from the
shell this process returns to.

`restore` runs on `Drop` too, so a panicking caller must not leave the terminal in
raw mode.

## next_completion

The token bounds come from the same split the completer used, which is why they
are asked for again here rather than returned with the candidates: two answers to
"where does this token start" would drift, and the drift would show up as text
inserted in the wrong place.

## apply_and_continue

A flag wants its value, and asking immediately is what turns Tab into "fill the
flag", Tab again into "fill the value" -- rather than one Tab and a letter typed
only to make the list reappear.

## Tui::stream

The block table lives in the log, not in `Tui`: there used to be a second copy
here (key -> line index) and a free function `stream_into` over it, which meant
the block model existed twice and could drift. `Log::stream` owns it now, and
`Tui::stream` only marks the screen dirty.

A closed stream must not append to a block that has since moved: `done` removes
the key before the next chunk looks it up, which is what makes two consecutive
responses end up as two blocks rather than one.

In place, and only the new bytes are wrapped: a delta must cost what the delta is
worth, not what the block has grown to.

An answer used to be the exception here, with its document kept in `Tui` as an
`Answer` and a `Drawn` decision saying whether to open or rewrite the block. The width
moved that decision into the log: a table's columns are the pane's, so a front end
that lays out before it knows the width cannot draw one, and a resize would have no
document left to lay out again. What is left here is one call for every kind, and the
cost -- one layout per delta for the block that holds a document -- is the cost the
accumulator paid anyway.
