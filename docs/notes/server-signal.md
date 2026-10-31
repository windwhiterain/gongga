# server-signal: how the server is asked to stop

A console window closes, a console sends Ctrl-C or Ctrl-Break, a shell sends
SIGTERM: every one of these asks the server to stop, and none of them should lose
the turn that is running.

`cancel` already knows how to stop: it preserves each live session's turn as a
pending row, lets the turn unwind, and only then breaks the listen loop. So the
signal path is not a second stop -- it is `commands::cancel`, reached from a place
with no client to answer.

## The handler only nudges

`SetConsoleCtrlHandler` runs on a thread the console owns, and a unix signal
handler may not take a lock. So both do the one thing that is safe from any
thread: `Notify::notify_one`. The async side awaits the nudge, logs, and runs
`commands::stop`, which is `cancel` with the answer written to the log. The
listen loop then drains as it always does and `serve` returns in the usual way,
so shells close and scripts stop on the existing path.

A nudge with nobody waiting is remembered by `Notify`, so a signal that lands
while the runtime is still starting is not lost.

## A second ask is the user insisting

After the first ask, `net::serve` waits for running turns with no deadline at all.
That is right for a turn that stops when cancelled, and wrong for a tool call that
ignores it: the console would stop answering. The second ask therefore leaves at
once with exit code 130. Nothing is lost by leaving then -- the pending row was
written by the first ask.

## The window that started the server

A server started by the TUI is started for that window, and `cli::spawn` names it
in `--parent`. When the window ends -- normally, by a crash, or by `taskkill /F` --
the server takes the same stop: the turn is preserved and the server exits, so a
killed window does not leave a process holding the port and the home lock.

Windows waits on the process handle, so pid reuse cannot fool it. Unix asks the
kernel every 250 ms whether the pid still exists, which a reused pid can.

Two callers deliberately pass no parent. `cli serve` execs the server in its own
console and watches it, and `ensure` starts one for a command that exits in
milliseconds: both servers have to outlive the process that started them.

## What a signal cannot fix

Killing a server itself (`taskkill /F`, `kill -9`) bypasses all of this: no
process can prepare for its own murder. What is left behind then is nothing --
the port and the home lock go with the process.

A window that leaves normally has already cancelled over the protocol, so the
parent watcher's ask finds `shutting_down` set and returns: one stop, whichever
source reaches it first.
