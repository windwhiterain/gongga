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
the list, a pane, or a line group. The three have different scroll behaviour, so the
part is part of the answer rather than something the caller re-derives.

## handle_mouse

The wheel scrolls what it is over, and retargets nothing: over a pane or a line
group it scrolls that session's rows, and over the session list it scrolls the list,
which has a scroll of its own (SPEC gives it one). The keys keep acting on the sticky
session whatever the pointer did.

A click is a different thing: SPEC makes it "set the sticky session", so the click is
turned into the command that does that (`/set_sticky --name session --value <name>`)
and handed to the loop as a submitted line. The rule stays in the command layer, and
the window keeps one path for everything the operator asks for. The leading `/` is
what makes it a command: a line without one is prose, and prose goes to the agent.

## Screen::sticky_session

Filled from `Sink::sticky_session` on every refresh, so the list's mark, the keys and
a click all read one value: the one the command layer has in `Sticky`.

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

## Screen::sticky_session is the only state

There is no `focus`: no "which pane did the pointer last touch", no second name that
can drift from the one the keys obey. The sticky session decides everything the
keyboard does -- which pane a page scrolls, whose header is highlighted, which pane
takes the rows an even share leaves over -- and a click is what changes it, by asking
the command layer for the command that sets it.

The wheel is momentary: it moves what it is over and writes no state, so a glance at
an older line does not retarget the next page key. A click sets the sticky, and that
is the only way the keys' target moves.

The session list has no scroll of its own either. Its window is derived from the
sticky session (`render::list_window`), which is what makes one name the whole of the
state: the list cannot drift to showing a different set of rows than the loop is
sending to.

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

`set_status` and `set_footer` only ask for a frame when the text actually
changed: the caller recomputes the status from the session list as events
arrive, and most of those recomputations say the same thing as the last one.

## Tui::restore_input

The counterpart of Enter: a line that could not be sent is not swallowed by a log
entry. It goes back where it was typed, because the operator already knows how to
press Enter again and does not want to retype it.

## Tui::stream

`key` identifies the stream (a response part, a tool's stdout) and `done` closes
it, so the next chunk with the same key starts a new line.

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
wired to scroll. Plain up/down stay with history -- that is what a shell does
with them, and it is the one thing operators try first.

Ctrl+<letter> would otherwise insert its letter: a terminal that does deliver
Ctrl+K here (some do) must not type a `k`.

Arrows belong to the completion popup, then to the text, and only then to
history: inside a multi-line message the line above is what up means, exactly as
in every other editor. Shift is what says "move the view, not the line".

A page is the log window, not a fixed ten rows: on a short terminal ten rows
scrolls past what is visible and the jump looks arbitrary.

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
moves. Over the session list it scrolls the list, which has a scroll of its own
(SPEC gives it one) because a list is not a session. Neither leaves state behind: the
keys still act on the sticky session.

A press is different, and it is the only reason a click does anything at all: it sets
the sticky session, as the command that does so. No click is an action on the
conversation, because everything that changes state is a command.

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

## layering::the_front_end_depends_on_the_command_layer

The arrow this crate exists to point: the front-end depends on the command layer,
never the other way round. A comment cannot enforce that; a manifest can be read.

## tests::Headless

The key handler is driven without a terminal, which is the only part of the UI
that has logic worth testing.

`rows` is every row the log holds, unwrapped at a width none of these cases
reach -- which is what the assertions are about.

## completing_a_flag_asks_for_its_value

Tab on a flag, then Tab again, fills the value too: the list has to come back on
its own once the flag is on the line, because a letter typed only to make it
appear is a letter that has to be deleted again.

## two_open_streams_do_not_mix

Two streams open at once have to stay separate: the index map is what lets a
tool's output and a response interleave without either one appending to the
other's line.

## alt_q_is_the_only_way_out

Leaving is the one thing the keyboard still does on its own, and Alt+Q is the only
key that does it.

Pinned down here rather than in prose because the TUI needs a real terminal to
run: without a test like this, "Alt+Q quits" is only a claim, and a binding that
came back unnoticed is a Ctrl key the terminal would have eaten before the
program ever saw it. The old bindings are gone: Ctrl+X and Ctrl+C mean nothing
now.

## enter_sends_and_the_modified_enter_breaks_the_line

Enter sends, the modified Enter breaks the line. Getting this backwards sends
half a message every time someone starts a new paragraph.

## wheel_notches_become_rows

The wheel is the scroll control everybody reaches for first, so what one notch
means is worth pinning down.

## scrolling_stops_at_both_ends

Page up pressed ten times must not leave the view past the top, and page down must
not walk off the bottom: both ends stop the scroll.
