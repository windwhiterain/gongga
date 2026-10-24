# tui/lib.rs — a paste that arrives as keystrokes

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

Verified by unit tests on both halves (`only_text_joins_a_run` for what joins a
run, `a_key_run_with_a_break_in_it_is_a_paste` for what a run means) and by the
existing `a_paste_is_inserted_not_submitted` for the insertion path. A real paste
into a real terminal is the open check: a ConPTY driver was attempted and hung
the shell twice, so it was removed rather than left as a trap.
