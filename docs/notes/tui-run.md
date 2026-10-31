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
command's name -- so `gongga-cli send_message ...` takes no `/`. Carrying the prefix
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

A cold start is not a fault and is not reported as one. The first attempt of an
outage says its line once: when the address refused the datagram and a server may
be started here, it is a system line -- `nothing is listening at 127.0.0.1:9700 --
starting one` -- because that is what is about to happen. Every other failure is an
error, since retrying is all this loop can do about it. The status line carries
`attempt N` from then on, so the outage is not narrated twice.


The one thing done before the window opens is `cli::home_of`: the loop needs the
home for the server it may start, and nothing else. The command an agent's shell
types is not this front-end's to write -- a shell only exists inside a server, and
the server writes it from its own program as it comes up (`cli-server.md`).

Starting a server is SPEC's job for this front-end, and it happens *before the first
dial*, not on the first failed dial. Asking first cost the first seconds of every
launch in a backoff nobody needed, and reported a fault that is not one: a window
whose server is three milliseconds behind was already saying "nothing is listening".

`starts_a_server` is the whole rule -- not `--client_only` -- and `cli::spawn` does
it without asking. The home's instance lock is what arbitrates when two windows open
at once: the loser's child exits and the winner serves.

Which process is "ours" is answered by pid, not by address. `spawn` returns the pid
of the child it started, the greeting carries the pid of whoever answers, and only a
match is cancelled on the way out. An address would say the wrong thing in both
directions: another window's server for the same home has the same address, and a
`connect_server` can move this window onto a server it did not start.

While a server this window spawned is coming up, the first failed dials are that and
nothing else -- a fresh home has no connection key until the server writes it -- so
the outage line is said only when nothing is starting. The status line still shows
`offline — reconnecting (attempt N)`, which is the truth.

Resolved before the terminal is taken over: a bad address is not fixed by retrying,
and an error is far easier to read on the normal screen than after `restore()`
flashed the alternate screen away.

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

A server this window started is stopped on the way out, through `cancel` rather
than a kill: `cancel` is SPEC's stop -- it finishes what cannot be serialised and
stores the rest, which a killed process cannot do. Only that server, and only
while the window is still pointed at the address it started: one somebody else
runs is not a window's to stop, and a `connect_server` that moved elsewhere must
not take the old one down with it. Once the link is gone there is nothing to ask
with, so the way out stops nothing.

Best-effort goodbye: the server drops this connection now instead of waiting out
its idle timeout. Worth nothing if it fails -- the window is already closing.
