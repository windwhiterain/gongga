# clipboard.rs

Putting text on the operator's clipboard, with whichever tool this machine has.

## Why a process and not a crate

A clipboard crate is a large dependency for one call, and each platform's
"correct" implementation is a moving target this crate does not want to own. The
platforms already ship a tool that reads stdin and puts it on the clipboard:
`clip` on Windows, `pbcopy` on macOS, `wl-copy` / `xclip` / `xsel` on Linux. One
`Command` reaches all of them, and a missing tool is a message rather than a
build failure.

## candidates

Ordered, and tried in order. Wayland before X11 on Linux because a Wayland
session's `xclip` either fails or writes somewhere the clipboard never reads.

## copy_with

The seam that makes this testable: `copy` is `copy_with(text, candidates())`, so
a test can exercise the spawn, the stdin pipe and the exit status against a tool
that touches nothing. A test that put text on the operator's real clipboard would
be a test with a side effect outside the repository.

## Copied

Four outcomes rather than a `Result`, because "no tool is installed" and "the
tool refused the text" are different facts and only one of them is a failure.
`Failed` carries the tool's own words: a copy that silently did nothing is worse
than one that says it could not.

## Invariant: a missing tool is skipped, not fatal

A Linux box with only `xclip` must not fail because `wl-copy` is absent. The
first candidate that runs and exits zero wins; the rest are never spawned.

## readers

The same idea in the other direction: `Get-Clipboard` on Windows, `pbpaste` on
macOS, `wl-paste` / `xclip -o` / `xsel -o` on Linux. They print the clipboard on
stdout, which is what a paste needs.

Only reached when a terminal forwards Ctrl+V instead of pasting the text itself:
Windows Terminal and VS Code both paste on their own, so this path is for the
others, and a `powershell` startup is why it is not the one everybody takes.

## FromClipboard

Four outcomes, matching `Copied`: an empty clipboard is not a failure and has no
words to say, a missing tool is, and a tool that refuses carries its own message.

## paste_with

The same seam as `copy_with`: `paste` is `paste_with(readers())`, so a test reads
a real process's stdout without touching the operator's clipboard.

## paste_text

Normalizes `\r\n` and lone `\r` to `\n`. A Windows clipboard hands back CRLF, and
a bare `\r` inside the input box renders as a glyph rather than a break. Nothing
else is touched: tabs and trailing spaces are the operator's text.

## a_paste_keeps_its_lines

The breaks survive. In the paste path a newline is text, not a keystroke — see
`Tui::paste` in `tui-lib.md`.

## a_failing_tool_is_reported

The negative of the happy path, and the one that bites silently: a tool that
exits non-zero must not read as a successful copy.

## a_read_reports_what_the_tool_printed

Three outcomes in one test: a tool that prints is text, a tool that prints nothing
is `Empty` rather than a failure, and a tool that is not installed moves on.

## The image, and the tools that read it

A clipboard can hold a picture as well as text, and the same Ctrl+V that reads
the text reads the picture. The picture is read first, because a screenshot
copied out of an image tool usually carries both formats and the picture is what
the operator meant; the text read is the fallback and is unchanged.

`ImageClipboard` has four answers, matching `FromClipboard`:

| answer | meaning |
| --- | --- |
| `Saved(path)` | the image was written to that file |
| `NoImage` | the clipboard holds no picture — it may still hold text |
| `NoTool` | no image-capable clipboard tool could be run at all |
| `Failed(why)` | a tool ran and refused; the message is its own words |

`NoImage` is not a failure: it is the ordinary answer for a clipboard holding
text, and the caller falls back to the text read. `NoTool` and `Failed` are
reported by the *text* branch, in the text branch's own words, so one keystroke
never produces two competing complaints on the status line — see
`docs/notes/tui-paste.md`.

## image_candidates

The per-platform picture tools, and there are fewer of them than there are text
tools, because reading a picture out of a clipboard is not something every
platform ships a command line for:

- **Windows**: `powershell -NoProfile -STA -File <script> <path>`. It has to be
  `-STA`, or `[Windows.Forms.Clipboard]::GetImage()` throws. The one-liner is
  written once to a `.ps1` file under the temp directory and run with `-File`,
  because `-Command` does not hand trailing arguments over as `$args`, whereas
  `-File` does: the destination path arrives as `$args[0]`. The script exits 0
  and prints nothing when the clipboard holds no picture.
- **macOS**: `osascript -e <AppleScript>`, which writes the clipboard's PNG to
  the destination file itself.
- **Linux**: `wl-paste -t image/png` and then `xclip -selection clipboard -t
  image/png -o`, Wayland first for the reason `candidates` gives.

The first two write the file themselves; the Linux pair print the image bytes on
stdout, which the caller redirects into the destination file. The distinction is
`ImageTool`'s two modes, and it is the reason a picture is never read as text.

## dest_dir

Where a pasted picture is written: a `gongga-tui` directory under the system
temp directory, created if it is missing. The file name is generated
(`gongga-paste-<pid>-<n>.png`) rather than taken from the clipboard, which
matters twice over: the name has no spaces, so the `@image:` reference the
prompt shows is one word and the server's Meta parser reads it whole; and the
picture outlives the paste, because the message references the file at the moment
it is sent.

## paste_image_with

The same seam as `paste_with` and `copy_with`: `paste_image` is
`paste_image_with(image_candidates(), &dest_dir())`, so a test can exercise the
spawn, the file write and the exit status against a tool that touches nothing.

## A tool that writes nothing means no image

A clipboard tool that runs, exits zero and produces no file has told the truth:
the clipboard holds no picture. That is `NoImage`, not a failure, and the
destination file is removed rather than left behind as an empty picture.
