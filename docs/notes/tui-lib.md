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

The `gongga` binary lives here because it is the composition root: the only
program that needs both halves, and linking them is what a binary is for.

## cli re-exports

The vocabulary belongs to the command layer: this crate paints whatever it is
told to paint. Re-exported so `crate::LineKind` and friends keep reading the
same from inside this crate -- the renderer and the log never have to know where
the words came from.

## UiEvent

There is deliberately no "cancel" here. Stopping work is a command like any
other (`interrupt_session`, `cancel`), so it reaches the server by the same path
as everything else and can be typed by a script that has no keyboard. The only
thing left to the keyboard is leaving.

A `Submit` may contain newlines: a message with paragraphs is still one message.

## Screen

Everything the renderer reads. Kept separate so drawing never borrows the
terminal while the caller is mutating state.

## Screen::log

The scrollback. Its own type because a frame must not be allowed to walk it: see
`crate::log` for why that is the difference between a usable window and a frozen
one.

## Screen::scroll

How far the log is scrolled: 0 is pinned to the newest line, `n` is `n` wrapped
rows up into the past.

## Screen::max_scroll

How far up the log *can* be scrolled right now, written back by the renderer,
which is the only place that knows how many rows the lines wrap into and how
tall the log window is. Scrolling keys clamp against it so a page-up cannot
strand the view past the top.

## Screen::scroll_by

Positive scrolls into the past, negative back towards the newest line, and both
stop at the ends. The upper bound is the renderer's, since only it knows how many
rows the lines wrap into.

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

Copies the rows currently drawn, measured exactly as `render::draw_log` measures
them — one column short, without the chrome rows. A copy that took a different
width would put back text the screen had wrapped differently.

## Tui::toggle_mouse

Holds no opinion about which mode is better, and flips a flag only after the
terminal accepted the change: a flag out of step with reality makes the next
toggle lie.

# Invariant

Releasing mouse capture is what makes native copy possible at all. While the app
holds the mouse, the terminal never sees a drag and cannot select — which is why
"the TUI cannot copy" was true before this key existed.

## Tui::handle_mouse

The wheel is the scroll control everybody tries first. A notch is three rows;
Shift slows it to one, the way a terminal's own scrollback behaves. Nothing else
the mouse does is wired to anything: clicks are not actions in a command line.

## Tui::wheel_rows

What a wheel event meant, in rows: positive is into the past. `None` for
everything else the mouse can report, none of which is an action here.

## Tui::page_height

The input's height depends on what has been typed into it, so the log's does too:
a page has to be measured against the window that is left, not against a
constant.

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
