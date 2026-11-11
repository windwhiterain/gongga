# tui/lib.rs — a paste arrives whole

## The console says when it is a paste

`EnableBracketedPaste` asks the terminal to wrap a pasted block in
`ESC[200~ … ESC[201~`. On Windows, crossterm 0.29 parses that sequence only in
its unix reader: its console reader walks `ReadConsoleInput` records
(`KeyEventRecord`) and has no paste variant at all, so the sequence asked for
would reach nothing that understands it.

`vt_input::Input` opens the one reader this window uses for its whole life. On
Windows it sets `ENABLE_VIRTUAL_TERMINAL_INPUT` and parses the console's byte
stream into `crossterm` events; a bracketed block becomes one
`Event::Paste(text)`. Elsewhere crossterm's own `EventStream` already reports
the same event. Neither reader replays a paste as keystrokes, so a line break
inside a pasted block is text, never an `Enter`.

`Tui::paste` inserts the string whole through `Screen::insert_paste`: the
completion popup closes first, because it was about the token before the paste.
A plain `Enter` is the only key that submits; `Shift+Enter` and `Alt+Enter` are
the operator's own line breaks. Nothing between a key arriving and what it
means is guessed from timing.

## Win32 input mode: the one key the console cannot spell

The console's VT translation writes a modified `Enter` as a plain `CR`. SPEC's
`Shift+Enter` is a line break, so in the byte stream it was indistinguishable
from the key that submits, and the break never happened. No parser can recover
what the console did not write, so the window asks for a mode that spells it:
`DECSET 9001`, Win32 input mode, one record per key event --
`Vk ; Sc ; Uc ; Kd ; Cs ; Rc` between `[` and `_`. `Shift+Enter` is
`13;28;13;1;16;1_` and nothing else.

Both consoles on Windows implement it: the console host and Windows Terminal
answered the request with records in a run measured on 2026-11-11. `vt_input`
asks for the mode before the first frame and gives it back on the way out, so
the shell this window returns to reads its own keys.

A record is read as what it says, and nothing is guessed:

- the key its virtual-key code names, with the modifiers `dwControlKeyState`
  reports -- `Shift+Enter`, `Alt+Enter`, `Shift+Delete`, `Shift+Up`;
- else the character it carries, with those modifiers -- a control byte is the
  letter the console means, and a character outside the BMP is two records, the
  first half held until the other arrives;
- else nothing: a release, a modifier on its own, a key with no name.

A record with no virtual-key code is not a key at all: a program wrote bytes to
this console. A run of those records is put back into the byte stream where
they were, so `ESC[D` written that way is still one arrow rather than an `Esc`
and two characters. For the same reason a record never finishes an escape an
earlier batch left open -- a record is whole, so the unfinished escape goes.

What the mode costs: with it in force the console host unwraps a terminal's own
bracketed paste into plain keys, so that paste arrives as typed text and a line
break inside it submits. SPEC's paste is `Ctrl+V`, which is this window reading
the clipboard (`Ctrl+V` arrives as the key it is), and it is unaffected. A
terminal that replays a paste as keys already had this failure mode, below.

## Why the key-stream rules were removed (2026-11-11)

Before the byte reader, a paste arrived as ordinary keys and the window had to
guess which `Enter` was a pasted line break. The guesses accumulated:

- a run of keys held in `Tui::pending` until a `PASTE_GAP` quiet gap;
- `MACHINE_RUN`, `GLUE_GAP`, `PASTE_BURST` and `BURST_GAP`, which glued a lone
  `Enter` to a recent machine-fast run;
- `PasteMarkers`, which rebuilt the bracketed markers out of six ordinary keys;
- `is_paste_newline`, which accepted an `Enter` carrying `CONTROL` as a paste's
  break rather than a send.

They were kept when the byte reader became the default, so both answers were in
force at once. The reported symptom: typing a few characters and pressing
`Enter` quickly was judged a paste's newline, so the message was not sent. The
rules only ever guessed, and the reader now knows. All of them were removed,
together with the `GONGGA_TUI_VT_INPUT=0` switch back to the console-record
reader: keeping that switch would have left a mode in which a pasted newline
submits a line at a time.

`QuickEdit` stays. It is not a heuristic: with the console's QuickEdit on, a
right-click puts the clipboard into the console's input stream behind this
window's back, and a line break in it would arrive as an `Enter`. Turning the
mode off stops the console delivering that gesture at all.

Two things had to survive the removal, because the old `flush` did both:

- `next_event` sets `self.dirty = true` for every key. `draw` skips a frame
  nothing changed for, and without it the line a submitted `Enter` cleared
  would stay on screen.
- `completer.wants()` still produces `UiEvent::Recomplete` after a key, so a
  completion that needs the loop to reach the connection is not lost.

## A sequence may arrive in pieces

The console writes one gesture in more than one `read`: `ESC` alone, then
`[<64;50;24M`. Reading the first batch as a key turned a wheel turn into an Esc
and then the twelve bytes of the sequence typed into the prompt. The parser now
holds a trailing escape and waits 50 ms for the rest; only that silence makes
it the Esc key, and what is left of an unfinished sequence is dropped rather
than typed. The same buffer already waited for a half-arrived paste, so this is
that rule applied to the other sequences too. `GONGGA_TUI_RAW_KEYS` names the
file the raw bytes and parsed events go to, made fresh each run.

## What a paste still cannot survive

A terminal that answers `EnableBracketedPaste` by replaying the text as plain
keys, without the markers. Nothing separates those keystrokes from typing, and
this window says so rather than pretend: the first `Enter` sends. `Alt+V` reads
the clipboard for an operator in such a terminal.

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
