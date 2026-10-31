# tui/lib.rs — a paste that arrives as keystrokes

## A pasted newline is not a plain Enter

The timing rule above guesses a paste from how fast keys arrive. It cannot be the
whole answer, because what a terminal sends for a newline inside a pasted block is
not the key a hand presses: **Windows delivers it as `Enter` with `CONTROL`**, read
off this window's own console rather than guessed.

Two failures follow from treating that as a bare Enter:

- a rule that only accepts the unmodified key drops the break, and the block arrives
  as one run-on line;
- a rule that treats any Enter inside a run as a send posts the block a line at a
  time -- the report this was fixed for.

`is_paste_newline` therefore accepts an Enter carrying neither `SHIFT` nor `ALT`, and
`glue_break` treats a `CONTROL`-carrying Enter as settled on its own: a hand does not
produce that key to send, and the timing rule cannot see it anyway -- ConPTY hands
pasted keys over one at a time, slower than `PASTE_GAP`, so a run of them never
accumulates. The timing rule stays for terminals that replay a paste as fast
keystrokes.

## The problem

`EnableBracketedPaste` is written to the terminal, and `Event::Paste` is handled
when it comes back. On Windows it never comes back: crossterm 0.29 parses
`ESC[200~ … ESC[201~` only in `src/event/sys/unix/parse.rs`. The Windows reader
walks `ReadConsoleInput` records (`KeyEventRecord`) and has no paste variant at
all, so `EnableBracketedPaste` there writes a sequence nothing will ever read.

What the terminal does send is the pasted text key by key. Every `\r` in it is
an `Enter`, and `Enter` is the key that sends, so pasting a block posted its
first line and pushed the rest into the conversation one turn at a time. That is
worse than not supporting paste: it looks like the harness repeating itself.

## The fix: a run of keys

Keys stop being acted on immediately. One is held in `Tui::pending`, and the loop
waits up to `PASTE_GAP` (10 ms) for the next:

- **Another text key** joins the run, and the wait starts again from there.
- **A pause, a `Paste`, a mouse move, a resize, a binding key** ends it.

Then `pasted` decides what the run was:

- **One key** is a keyboard. `Enter` alone still submits; typing is unchanged.
- **A run carrying an `Enter`** is a paste: it is inserted as text, each `Enter`
  becoming `'\n'`, and nothing is sent.
- **A run without an `Enter`** is fast typing. The keys are replayed through
  `handle_key` exactly as before, so a quick `/se` still opens the completion
  popup. Only sending had to be prevented, not typing.

The window is a cadence, not a budget, and it sits below what a keyboard can do:
key travel plus release is well over 20 ms even when two keys roll into each
other, while a paste is one write from the terminal and arrives with no gap.

## Why the run lives in `self`

`next_event` is one branch of a `select!` in `run.rs`, so it is *cancelled* every
time the wire wins. Collecting keys in a local `Vec` inside the future would drop
them on exactly the wrong occasions -- a burst is most likely while a response is
streaming, which is when the wire wins most often.

So `pending` is a field: a cancelled wait resumes the run next round with the
keys it already had. The event read itself is cancel-safe (`EventStream::next` is
a channel receive), so nothing is lost either way.

## The repaint flag

The first version of this lost `self.dirty = true`, which used to live in the
key branch of `next_event`. A held key is only turned into an edit inside
`flush`, so without the flag nothing marked the frame dirty -- and `draw` skips a
frame nothing changed for. The characters were in the input line the whole time;
the screen simply never showed them, which reads as "the keyboard stopped
working" rather than as a rendering bug.

`flush` now sets it unconditionally, and the `repaint` module holds that line by
reading the syntax of `flush` rather than by driving a terminal: this is exactly
the class of bug a headless test cannot see, and the one that must not come back
silently.

## The break that arrives alone

Reported in use: pasting a five-line block sent five messages, one per line.
The transcript showed five `user` rows inside the same second, each equal to one
line of the block -- so every break had been taken for a submit.

A terminal that pauses at each break delivers `line`, `Enter`, `line` as three
runs, not one: the run of line characters closes on the timeout, and the `Enter`
then arrives as a run of exactly one key, which is what a break typed by hand
looks like. Widening `PASTE_GAP` alone would not be safe -- it is also what
decides whether a human's fast typing is taken for a paste.

What separates the two is not the gap but **who could have produced the run**:
`MACHINE_RUN` (3) text keys inside `PASTE_GAP` (10 ms) is faster than a keyboard
and slower than nothing. So a bare `Enter` arriving within `GLUE_GAP` (150 ms) of
such a run is that paste's own break, and becomes `'\n'` in the prompt instead of
a submit.

The residual risk is a human who rolls three keys together and then hits Enter
within 150 ms. It costs them one extra Enter, and it is the safe direction: the
alternative is one message per pasted line.

That rule needs the run to accumulate, and in this harness's own console it does not:
ConPTY delivers a pasted block one key at a time, slower than `PASTE_GAP`, so the
run is always one key long and the timing test never fires. What does reach the
window is the *modifier*: a pasted newline arrives as `Enter` with `CONTROL`, which
is the subject of the first section. Both rules are kept, because the terminals
differ: one for a paste replayed as fast keystrokes, one for a paste that says so.

## What the run holds back

Whatever ended a run is returned as `Tui::stashed` and read at the top of the
next round: a `Paste`, a mouse event or a resize that arrived behind a burst is
delayed, never dropped. That is the one bug this shape could cause, and the
reason it is a stored value rather than a `continue`.

Modifier keys break a run rather than joining one: `Alt+Enter` and `Shift+Enter`
are the line break, `Alt+K` kills to end of line, and Ctrl+letter is swallowed on
purpose. `only_text_joins_a_run` holds that line.

## What this does not fix

The `ESC[200~` markers themselves. They do not reach this code on Windows --
what arrives is the text, which is why the run is all it takes. A terminal that
both sends markers *and* has no paste parser would put `[200~` in the prompt; no
such case has been observed, so guessing at a stripper would be guessing.

A paste delivered in one- or two-key runs with pauses between them is still
indistinguishable from typing; nothing here can fix that, because the terminal
sends no marker. Verified by unit tests on both halves (`only_text_joins_a_run` for what joins a
run, `a_key_run_with_a_break_in_it_is_a_paste` for what a run means) and by the
existing `a_paste_is_inserted_not_submitted` for the insertion path. A real paste
into a real terminal is the open check: a ConPTY driver was attempted and hung
the shell twice, so it was removed rather than left as a trap.
