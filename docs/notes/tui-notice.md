# tui/notice.rs

## What it is for

A session that stops is a fact the window can miss: the operator is reading
another session, or the window is behind something else. The toast says it on the
desktop, where the operator is looking when they are not reading the window. The
same need the DSH plugin `dsh-turn-notify` answers on the machine it runs on --
the mechanism is borrowed from it, the wording is this harness's.

## Why PowerShell

Windows has no toast a program can raise without one of three things: a
registered application id, a file on disk, or a shell that can speak WinRT. The
first two were both refused: a registered id means a shortcut in the Start menu,
and that file is exactly what a shortcut-dropper looks like to an antivirus (the
DSH plugin hit `HEUR:Trojan/LNK.Agent.b` for it and removed the route). The
third is what this uses -- `powershell.exe` with the script in
`-EncodedCommand`, which is a stock Windows binary doing a stock Windows thing.

The script is UTF-16LE base64 because that is what the flag decodes, and because
a script handed over on the command line has to survive quoting in every shell
that might ever run it. Encoding it also means the title and the body -- a
session name, a sentence -- travel as data: the XML they go into is escaped,
and the PowerShell string they sit in doubles its quotes. A name with an
apostrophe in it is a name that works.

## The id it shows under

`gongga`, which Windows has never heard of. An unregistered application id is
displayed as the string itself, so the notification centre reads "gongga"
without a registry entry, a shortcut, or a file of any kind. The notification
centre writes a settings key for it the first time -- that is the shell's own
doing, not ours, and it is the same key the DSH plugin's `dsh` has.

## Nothing is waited for

The toast is a child process, and the window does not wait for it: the shell
takes a second to start, and the window has a frame to draw in the meantime. A
toast that cannot be raised is reported on the status line and otherwise
forgotten -- it is a courtesy, and a courtesy that blocks the window is worse
than no courtesy at all.

The child's output goes to null. A window is a terminal of its own, and a toast
that printed its PowerShell progress into the operator's screen would be worse
than no toast.

## A machine with no toast

The module is one function on Windows and a no-op elsewhere, so the crate builds
and runs the same everywhere. A machine that is not Windows has no notification
centre to answer, and a window that says nothing is the honest answer there.
