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

## Alt+V, for the terminals that forward it

SPEC makes `Alt+V` the paste. Bracketed paste is the other half of the same
feature: a terminal that pastes by itself (Windows Terminal, VS Code) never sends
the key at all, it sends the text. A terminal that forwards the key gets the
clipboard read here instead — `clipboard::paste()` — and the answer goes in
through `Tui::paste`, so both roads end in the same place and neither replays
keystrokes. `Ctrl+V` is read the same way, for the terminals that forward `^V`.

Nothing is said when the clipboard is empty. When there is no tool to read it, or
the tool refuses, the status line says so and the prompt is untouched: a paste
that quietly did nothing is the failure mode this note keeps coming back to.

## Why the spelling is Alt+V and not a Ctrl one

VS Code's terminal binds `Ctrl+V` to `workbench.action.terminal.paste`, and that
command is in the default `commandsToSkipShell` list: the key is taken before the
terminal sees it, so this window never gets a key event -- not even the text, if
the clipboard holds only a picture, because VS Code's own paste reads text. An
operator inside VS Code then has no key at all that pastes a screenshot.

`Ctrl+Alt+V` was this note's first answer to that, and it does not work in VS
Code either. What reaches a program is measured twice: a console program that
prints every `ReadKey` record, and a trace inside this window's own key handler.
Both were fed the same keystrokes in the same VS Code window:

| keystroke | reaches a console program | arrives as |
| --- | --- | --- |
| `Ctrl+V` | no -- VS Code's paste takes it | -- |
| `Ctrl+Shift+V` | no -- VS Code's paste takes it | -- |
| `Ctrl+Alt+V` | no | -- |
| `Ctrl+Alt+C` | no | -- |
| `Alt+V` | yes | `Char('v')` with `ALT` |
| `Alt+Y` | yes | `Char('y')` with `ALT` |
| `Ctrl+Y` | yes | `Char('y')` with `CONTROL` |

The same probe in Windows Terminal delivers `Ctrl+Alt+V` as `ALT | CONTROL`, so
the console layer is not what drops it: a ConPTY client of a terminal that sends
the byte receives the byte. VS Code does not send it. xterm.js reads any
`Ctrl+Alt` keydown on Windows as a third-level shift -- AltGr -- and waits for a
character the browser never composes on a US layout, so nothing is written to
the pty at all.

`Alt+V` is therefore SPEC's binding: `clipboard_paste` accepts `ALT` with `v`,
and `CONTROL` too, for a terminal that forwards `^V`. Alt alone is the binding,
not Alt as a tolerated extra -- a Ctrl spelling leaves an operator in VS Code
with no key that pastes anything.

Which hop is measured: everything in the table above, and the paste itself --
in a real VS Code terminal `Alt+V` writes `gongga-paste-<pid>-<n>.png` and
inserts `@image(<path>)` into the prompt. What is reasoned is only why xterm.js
drops the key, which no line of VS Code's bundle was read to confirm; the
observation is that nothing arrives, and the fix does not depend on the reason.

### Keeping the Ctrl+Alt+V habit

A keybinding wins over xterm.js's AltGr handling -- measured, same probe -- so one
line in `keybindings.json` sends the byte directly and the operator keeps pressing
`Ctrl+Alt+V`:

    {"key": "ctrl+alt+v", "command": "workbench.action.terminal.sendSequence",
     "args": {"text": "\u0016"}, "when": "terminalFocus"}

`\u0016` arrives as `Char('v')` with `CONTROL`, which is the forwarded-`Ctrl+V`
path and needs nothing new from this window. `\u001bv` also works -- it arrives as
SPEC's `Alt+V` and is the plainest to write.

`commandsToSkipShell: ["-workbench.action.terminal.paste"]` also works and keeps
the plain `Ctrl+V`, but it applies to every terminal in the window: `^V` stops
being readline's quoted-insert in ordinary shells too. The keybinding is scoped
to the terminal and is the one this note recommends.

## Alt+V, and a picture

The same key pastes a picture, and the picture is tried first.

`Tui::paste_from_clipboard` asks the clipboard for an image before it asks it
for text. The order is the whole design: a screenshot copied out of an image
tool usually carries *both* formats — the picture and, often, a file name or a
URL as text — and the picture is what the operator meant to paste. Asking for the
text first would put that file name in the prompt and lose the picture.

A clipboard that holds only text still takes the image read, which answers "no
picture here", and then the text read runs exactly as it did before this
existed -- unchanged, including the empty clipboard that says nothing. A text
paste is not a second-class paste.

## The picture is a reference, not a picture

What goes into the prompt is the text `@image(<path>)`, and only that. The path
is where the clipboard module wrote the image; it is absolute, and the server
resolves an absolute path as it stands.

This is SPEC's Meta mechanism, and it was already there: a message carrying
`@image(<path>)` has the reference replaced by a file id when the message is
sent, the picture uploaded through the File API in advance of the request. The
TUI's whole part in this is to write the spelling the server's parser reads.
`image_reference` in `tui/lib.rs` is that spelling and nothing more: it prints
the path it is given, so a Windows path stays a Windows path and a POSIX path
stays a POSIX path.

The value is what stands inside the parentheses, whatever it holds: a path with a
space in it is written as it is, because the `)` is what ends the name rather than
the next space (`docs/notes/server-meta.md`). Nothing needs escaping here, and the
generated file name is still used rather than the operator's own, so the reference a
prompt shows names a file this home made.

Nothing is uploaded by the TUI and nothing is resolved by it. The reference is
text in the input line until the operator sends the message, and the server does
the rest. The picture therefore never reaches the model as bytes in the prompt,
and a message that only mentions a picture stays a mention until it is sent.

## How a picture gets into the input line

Through the same insertion a text paste uses, and not through `Tui::paste`.

`Tui::paste` runs `clipboard::paste_text` over what it is given, which
normalizes line endings — harmless for an `@image(...)` reference, but the semantics
of "paste" are "text the terminal sent", and a reference is not that. So the
shared part is factored out into `Screen::insert_paste`: it closes the
completion popup (the popup was about the token before the paste, not about what
the pasted text means now) and inserts the string whole. `Tui::paste` calls it
after normalizing; the picture path calls it with the reference.

Inserted whole, never key by key, and never submitted: the reference sits in the
prompt until the operator presses Enter, which is when the server sees it.

## A failure is reported once, by the text branch

The image read has four answers, and three of them mean "there is no picture to
paste": no picture on the clipboard, no tool on this machine that could read one,
or a tool that ran and refused. None of them is reported by the image path.

They fall through to the text read, and the text read is where the reporting
lives — `paste: no clipboard tool here; the terminal's own paste still works` or
`paste: <the tool's own words>`. So:

- the image read failed and the text read worked: the operator gets their text,
  and no second, competing error line;
- both failed: the status line says what the *text* read said, in the words the
  text read used.

The alternative — the image branch reporting its own failure on top of the text
branch's — would put two complaints on one status line for one keystroke, and
the second one would say less about what actually happened.

There is one case where the text branch has nothing to say and the image
branch's words are the only account of what happened: the image tool refused,
and the clipboard held no text either. Then the refusal is shown, in the tool's
own words. An empty clipboard is otherwise silence — nothing to paste has no
words to say — but a refusal is a fact, and a fact the operator can act on.
`image_failure_of` holds that rule, and `a_refused_image_read_is_reported`
pins it.

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
sends no marker. A real paste into a real terminal is the open check: a ConPTY
driver was attempted and hung the shell twice, so it was removed rather than left
as a trap.

## The burst that crosses runs

A right-click on a long clipboard posted it a line at a time, and the rule above
could not see it: that rule asks how many keys landed in *one* run, and a
line-by-line replay gives every character a run of its own -- `MACHINE_RUN` never
arms, so each line's `Enter` was read as the operator pressing send.

The loop now also counts keys that arrive back to back, in
[`BURST_GAP`](../../crates/tui/src/lib.rs) each, and keeps the count across
flushes. A lone `Enter` after [`PASTE_BURST`](../../crates/tui/src/lib.rs) of them is
the paste's own line break whatever runs they landed in, and the count is spent when
it answers, so one paste cannot excuse the `Enter` a person presses after it.

The two numbers are chosen so that no hand can arm it: twenty keys at most forty
milliseconds apart is twenty-five characters a second, held without a pause. A paste
is the only thing that reaches it, which is what makes "a copy never sends anything"
true rather than likely.
