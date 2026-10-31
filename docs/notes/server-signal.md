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

## What a signal cannot fix

Killing the process (`taskkill /F`, `kill -9`) still bypasses all of this. The
server that a killed client leaves behind keeps the port and the home lock until
something cancels it, because `net::serve` has no idle timeout. That is the open
question in `docs/notes/server-main.md`, not something a handler can answer.
