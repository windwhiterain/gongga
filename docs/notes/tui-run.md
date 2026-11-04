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

* `/` begins a command: `/send_message main hi`.
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

A cold start is not a fault and is not reported as one. While a server this
window spawned is coming up, the first failed dials are that and nothing else --
a fresh home has no connection key until the server writes it -- so the outage
line is said only when nothing is starting, and the status line carries
`offline -- reconnecting (attempt N)`, which is the truth.

When nothing is starting -- the window was told not to start one
(`--client_only`), or the spawn failed -- the reason is said on *every* attempt,
not on the first. The status line holds the newest thing this window knows, and
the refresh that follows a note wipes a note said once, so a reason said once is
a reason nobody sees. A window that says `reconnecting` and nothing else is a
window nobody can tell from a hung one. The line therefore carries the reason
and the way out: `nothing is listening at 127.0.0.1:9700 (attempt 3) -- still
trying; Alt+Q leaves`. It is the same words every time, so the outage is not
narrated twice -- one line replaces the last, it does not add to it.


## the loop (a burst is one frame)

A datagram is handled and then the loop awaits the next, and every turn of the
loop paints. That is right for a quiet link and wrong for a busy one: an agent
streaming deltas produces hundreds of messages a second, a frame costs tens of
milliseconds once the answer is large, and a loop that paints per message stops
reading the wire. The transport is one ordered channel with a window counted in
fragments, so a reader that stops reading makes the sender retransmit with a
doubling backoff -- and every command the operator types queues behind that. The
symptom is not a slow agent: it is a `send_message` whose own echo takes a minute,
only while the agent is busy.

So the wire branch clears everything already delivered before returning to the
loop, and one frame covers the burst (`Link::try_recv`). The accounting matters
as much as the draining: each message taken is one fewer pending, which is what
gives the peer the receive window back.

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

Which process is "ours" is answered by pid, not by address, and only two pids can
answer it: the one `spawn` returned (this window started that process, or the
watchdog above it) and the one the greeting carries as `owner` when it is this
window's own. The second is how a handover stays ours: `evolve` starts the successor
with the old server's argv, `--parent <window pid>` included, so a server that
arrives already running is still the one this window started. An address would say
the wrong thing in both directions: another window's server for the same home has
the same address, and a `connect_server` can move this window onto a server it did
not start.

Nothing on the wire claims a server. A dial used to carry the window's pid so that
an unowned server would follow it, which kept a handover under control but made
every server somebody had started by hand -- `serve`, the bundle's `server` -- die
with the first window that connected to it. Ownership is decided before any
connection now, so a window that merely reads a server leaves it running. See
`docs/notes/server-signal.md`.

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

A server this window started -- or a handover of it -- is stopped on the way out,
through `cancel` rather than a kill: `cancel` is SPEC's stop -- it finishes what
cannot be serialised and stores the rest, which a killed process cannot do.

# Invariant

What is stopped is what the header drew: `Server::is_ours` is the one answer, read
from the screen rather than recomputed, so `own #…` means "closing this stops it"
and `not own #…` means "it keeps running". That is also what makes a
`connect_server` safe: after one, the screen names no server, and once the link is
gone there is nothing to ask with anyway.

Best-effort goodbye: the server drops this connection now instead of waiting out
its idle timeout. Worth nothing if it fails -- the window is already closing.

## Two pids, one server

The pid `spawn` answers is the watchdog's and the pid in the greeting is the
server's, so `whose_server` accepts either match. With only the greeting's pid, a
window would print its own server as `not own #… (no owner)`, and the way out would
leave a server it had started. See `server-supervise.md`.

Pids and not an address: a `connect_server` can move the window onto a server it
did not start, and an address would then say the wrong thing -- the window would
read another build's answers as if they were its own.

## The four labels

The header says whether the server is own, and why, because the answer decides
whether closing the window stops it:

| label | meaning | closes with the window |
| --- | --- | --- |
| `own #pid` | the process `spawn` started | yes, by `cancel` |
| `own #pid (handover)` | a newer build in its place, carrying `--parent` | yes, by `cancel` and by the follower |
| `not own #pid (another window)` | started for another window | no |
| `not own #pid (no owner)` | started by hand, for no window | no |
