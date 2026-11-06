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

The nudge carries the words for the record: which console event, which signal, or
that the process this server follows ended. `commands::stop` reaches `cancel`
through `Origin::Signal`, so the `server_stop` row says `by the process this server
follows (pid 54552) ended` and not just "a signal" -- a reader asking who stopped
the server is answered from the log, and a signal has no client to name.

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

That stop does not let anything go. A detached job is spared by `cancel`, and
this is not one: the window went away without asking, so nothing promised those
processes a life after this server's. Their job objects close with the process
and take their trees with them -- which is what "kill the window and everything
under it" means to the user who did it. See
`docs/notes/server-tools-detached.md`.

Both platforms ask, every 250 ms, whether the pid is still running: Windows by
reading the handle's exit code, Unix by `kill(pid, 0)`. Windows used to *wait* on
the handle instead -- `WaitForSingleObject(handle, INFINITE)` inside
`spawn_blocking` -- and that was a stop that did not stop: `Runtime::drop` joins
the blocking pool, so a cancelled server kept its home lock until the window it
followed ended, which is minutes after the window was restarted and forever for a
window that was never closed. The port and the turn were already released by
then, which is what made it look like a handover that hung rather than an exit
that waited. A wait that outlives the stop must not be work `Runtime::drop` joins;
nothing is lost by asking instead of waiting, because the pending row is written
before a turn starts (`commands::deliver`), not when the server leaves.

## Who owns a server: `--parent`, and nothing else

`--parent` names the process whose lifetime this server shares. That is the whole
of ownership, and it is decided before the server serves its first connection.

A handover keeps it: `evolve`'s successor is `exec`ed by a launcher nobody typed
into a window, and it carries the old server's argv -- `--parent` included -- so a
server that arrives already running is still the one its window started, and
nothing has to watch it on the window's behalf.

There used to be a second way to acquire a server: `signal::adopt`, where the first
window to connect to an *unowned* one took it, so that a server left by a handover
would not outlive every window. It made every server somebody started by hand -- a
`serve` in a terminal, the bundle's `server`, a command's `ensure` -- stop when the
first window connected to it closed, which is the opposite of what an operator
means by starting a server. The handover was the only case it was for, and
`--parent` already covers it, so the claim is gone: a client reads, and a dial is
not a way to come to own something.

A server started by hand therefore belongs to nobody, and outlives every window
that reads it. Two callers deliberately pass no parent: `cli serve` execs the server
in its own console and watches it, and `ensure` starts one for a command that exits
in milliseconds -- both have to outlive the process that started them.

## What a signal cannot fix

Killing a server itself (`taskkill /F`, `kill -9`) bypasses all of this: no
process can prepare for its own murder. What is left behind then is nothing --
the port and the home lock go with the process.

A window that leaves normally has already cancelled over the protocol, so the
parent watcher's ask finds `shutting_down` set and returns: one stop, whichever
source reaches it first.

## A second process, and the same pids

The server normally runs under a supervisor (`server-supervise.md`), and this path
is unchanged by it: `--parent` still names the window, because the child gets the
same argv, so the watcher below watches the window and not the supervisor. `cancel`
and a dead owner still end in exit code 0, which is what tells the supervisor not
to start the server again.
