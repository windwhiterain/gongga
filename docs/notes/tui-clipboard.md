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
