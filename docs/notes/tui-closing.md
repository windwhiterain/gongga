# tui/closing.rs

## The ask a console makes on its way out

On Windows, closing a terminal is a handshake, not a kill. The host calls
`ClosePseudoConsole`, which sends `CTRL_CLOSE_EVENT` to every process still
attached to that console and then waits for them to leave. Before Windows 11
24H2 (build 26100) that wait has no bound at all.

The window is the process in the middle of that handshake, and it was the one
that could not answer: `next_event` is parked on an event a closing console will
never send, so the window stayed, the bundle head stayed behind it in
`Child::status` (`bundle-main.md`), and the head kept its own executable mapped --
`cargo install` then cannot replace a file that is still running.

`CTRL_CLOSE_EVENT` does not travel through the input buffer, so **raw mode has
nothing to do with this**. `ENABLE_PROCESSED_INPUT` -- the bit `enable_raw_mode`
clears, and the bit that turns Ctrl-C into `CTRL_C_EVENT` -- only governs input.
A window in full raw mode still receives close, logoff and shutdown.

## Where the ask is actually lost

Measured on 23H2 with `.dev/closing_probe.py`, a ConPTY that starts the real
window and then calls `ClosePseudoConsole`:

| the host | what the window sees |
|---|---|
| keeps reading the console output | `CTRL_CLOSE_EVENT` arrives, the window leaves in ~2s |
| stops reading, reads nothing | nothing arrives; `ClosePseudoConsole` never returns |
| stops reading, starts again 3s later | the event arrives the moment reading resumes |

`ClosePseudoConsole` does not deliver the ask until the console's output pipe has
been drained -- Microsoft documents the requirement ("continue reading from the
pipe until after `ClosePseudoConsole` has returned") and that failing it "may
cause `ClosePseudoConsole` to wait indefinitely". A host that stops reading when
it closes a terminal therefore strands every client it was about to ask.

**No handler can be reached in that state**, which is why this module does not by
itself make a window closable from the outside: it makes the ask answerable, and
the ask still has to be delivered. A window closed by such a host is a host
defect, and the only thing a client can do about it is refuse to wait forever.

## watch

Installs `SetConsoleCtrlHandler` and says whether a console that goes away will
be seen. The handler is the shape `server-signal.md` describes, and it takes the
same three events: the console going away is not the user typing.

On platforms without a console handler a closed terminal sends `SIGHUP`, whose
default action already ends the process, so there is nothing to install and
nothing missing -- `watch` answers `true` there and the window says nothing.

A window that cannot install the handler says so once, before its first frame. It
draws and works exactly as before; what it loses is the answer to a console that
closes under it.

## handle

Runs on a thread the platform injects, and one thing follows as invariant:

- **Nothing here writes and nothing here waits.** The platform may end this
  process the moment the handler returns, the console it would write to is on its
  way out, and a write to a console whose host stopped reading blocks instead of
  failing. The handler wakes the loop and returns; the loop owns the way out.

That is also why the way out stays the ordinary one: the loop leaves the same way
Alt-Q leaves, so the link, the journals and the terminal are given back by the
code that always gives them back.

`CTRL_C_EVENT` and `CTRL_BREAK_EVENT` return the default action. Ctrl-C is the
window's own key -- raw mode already delivers it as a byte, and the input line
answers for it -- so taking it here would give one keystroke two owners. See
`tui-lib.md`.
