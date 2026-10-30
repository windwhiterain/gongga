# tui/run.rs

## run.rs

This is where the two halves meet. The command layer (`gongga-cli`) knows what a
line means and what to send; this crate knows what a terminal is. The loop below
is the only place that needs both, which is why it lives in the crate that
depends on the other one.

`TuiSink` is the seam in the other direction: the command layer renders through
`cli::Sink`, and this type is the implementation that paints a terminal. Nothing
above it decides colours.

## RECONNECT_MIN

How long the interactive client waits before redialling, doubling up to the cap.
The first retry is fast enough to be invisible across a `dev.sh restart`, and the
cap keeps a server that is down for an hour from spinning.

This is a *redial* cadence, not a budget on anything the agent does: the operator
keeps the window, the history, the input line and its scroll position throughout,
and Alt+Q still works while it backs off.

## submission

The grammar is decided by the first character -- the one piece of syntax an
operator has to know:

* `/` begins a command: `/send_message --session main --message hi`.
* Anything else is a message to the agent.

It lives here, not in the command layer, because it is a property of the one
input box: an input line has to mean *something* when the operator types prose
into it, so it needs a way to say "and now a command". A command line has no such
problem -- its argv is already tokens, and the process was started with the
command's name -- so `gongga send_message ...` takes no `/`. Carrying the prefix
over was not harmless: on Windows Git Bash rewrites a leading `/word` into a
`C:/.../` path before it is even an argument, and the whole line then became a
chat message to the agent.

## Dialing

A dial kept alive between loop iterations.

Boxed rather than awaited inside the branch: an unreachable server takes the whole
handshake timeout to give up, and waiting for it inside the loop would freeze the
keyboard for five seconds at a stretch -- the TUI would be unresponsive exactly
while the server is down, which is when the operator is most likely to be pressing
Alt+Q.

## main

The window's own entry point: it builds the runtime and hands it to `interactive`.
It lives in the library rather than in `main.rs` because the binary now has two
shapes -- a command line, which is the cli's, and no command at all, which is this
-- and the choice between them is one `if`.

## dial_now

The address is resolved on *every* attempt, not once per process. `connect_server`
moves it under a running loop, and a key only appears after a server has minted
one, so an endpoint resolved at startup is the one thing that cannot be right in
both of those cases.

## interactive

The window and the link are separate things. Nothing here makes the server's
presence a condition for the client to exist: the TUI opens first, dials from
inside the loop, and when the link dies it goes back to dialling instead of taking
the window down with it.

A restart therefore looks like two lines in the log -- `connection lost`, then
`connected` -- with the input line, the history and the scroll position exactly
where they were.

The one thing done before the window opens is `cli::prepare`: the command an
agent's shell types has to exist before a shell does.

Starting a server is SPEC's job for this front-end, and it happens on the *first
failed dial* rather than at startup: that is the same moment the operator learns
nothing is there, it covers a `connect_server` that moved to a server nobody
started, and `--client_only` has exactly one place to say no. The task is spawned
rather than awaited, so the window keeps taking keys while a first start migrates
a home and fetches its content.

Resolved before the terminal is taken over: a bad address or a missing key is not
fixed by retrying, and an error is far easier to read on the normal screen than
after `restore()` flashed the alternate screen away.

Every arm keeps the keyboard branch, so no state -- connected, waiting, or
mid-handshake -- can make the window stop answering keys.

Nothing left this process, so the text is still the operator's. Put it back rather
than log it away.

The channel task is gone but its half-open end had not been noticed yet. This
message may or may not have arrived, so it is reported, not silently re-queued.

Raced with the link going down between `submit` and here. Nothing was sent, so the
line goes back.

Decoding stays in the command layer, so the front-end never names a protocol type:
it hands over the bytes it read from the link and reads the result off the screen.

Finished either way: clear it so a later outage cannot poll a dead future, which
would panic.

Applying the greeting *is* the resync: from this moment the catalog, the session
list and the model on screen are current, which is what makes a reconnect
indistinguishable from a first start.

Once per outage: the status line carries "attempt N", so repeating this every few
hundred milliseconds would bury the log without adding information.

Best-effort goodbye: the server drops this connection now instead of waiting out
its idle timeout. Worth nothing if it fails -- the window is already closing.
