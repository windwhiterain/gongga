# tui/run.rs

## run.rs

This is where the two halves meet. The command layer (`gongga-cli`) knows what a
line means and what to send; this crate knows what a terminal is. The loop below
is the only place that needs both, which is why it lives in the crate that
depends on the other one.

`TuiSink` is the seam in the other direction: the command layer renders through
`cli::Sink`, and this type is the implementation that paints a terminal. Nothing
above it decides colours.

## Where the input line lives now

As of 2026-11-08 the `submission and filled` and `submission` sections, and the
`follow` part of `sync, follow and settle`, describe code that now lives in
`crates/client/src/window_input.rs`; `RECONNECT_MIN` and `dial_now` live in
`crates/client/src/runtime.rs`; `declare` and the `answer_*` functions live in
`crates/client/src/window_refill.rs`; `sync` and `settle` live in
`crates/client/src/window_sync.rs`. The sections stay for their invariants. See
`client-window-input.md` and `client-runtime.md`.

## RECONNECT_MIN

How long the interactive client waits before redialling, doubling up to the cap.
The first retry is fast enough to be invisible across a `dev.sh restart`, and the
cap keeps a server that is down for an hour from spinning.

This is a *redial* cadence, not a budget on anything the agent does: the operator
keeps the window, the history, the input line and its scroll position throughout,
and Alt+Q still works while it backs off.

## submission and filled

The line is answered here, in this order: a head the window owns
(`docs/notes/tui-command.md`) never reaches the command layer; anything else has its
sticky entries filled in first, and is then handed over as tokens.

Filling appends `--name value` for every argument the catalog declares and the line
left empty, so a value holding a space stays one token and nothing is re-ordered. What
the argument's published `accepts` refuses is left out *and said*: a default is not
something the operator stated, so it is not worth a refusal -- but silently dropping it
would hide that the entry is still in the file, and a later model may well take it.

What the window says about a line it filled -- a refusal, or an entry it left off --
is a `Note` record in the journal as well as a line on the status row: see
`docs/notes/tui-cache.md`.

Which session the line acts on is read off the line *after* the entries are in it --
`Client::session_named`, the same answer `follow` keeps -- and an entry is filled into
a line about a session that already exists only when that session's row already says
the same thing (`restates`): `--model`, `--thinking` and `--ctx_limit` are compared with
the published model, level and share, and a kind the client cannot check is not filled.
A fill that disagrees would rewrite a setting nobody stated on this line, and would
refuse a value stated on it -- the window's `--cache_tier 1h` is a tier of the model
the *window* runs, so filling `--model` into a session on another model fails the line
for a model the operator never typed. What is left out is said, in the status line,
like the values `accepts` refuses. A session the line creates has no row to agree
with, so it takes the entries as they stand: that is what the entries are for.

## the gesture branch

`UiEvent::Operate` is the window's own gesture, and it takes one step the typed
path does not: `command::operate` answers the operation against the state, and
`sync` puts what it changed where the frame and the completion read it -- before
the next event, so the very next frame draws the new sticky session or the window
just hidden. A gesture the server answers (`delete_session`) has nothing to sync
and goes out through `Client::apply` like any other message. See
`docs/notes/tui-command.md`.

## sync, follow and settle

Three steps, and each happens exactly once per submission or answer:

- **sync** puts the window's state where the drawing and the completion read it: block
  size, box edge, sticky session, the drawn sessions, the completion's vocabulary.
  Called after anything that could have changed them.
- **follow** records what an accepted command will keep: a session the command names
  is drawn and becomes sticky (so the window draws what it was told about), and a
  `set_session` is remembered as `(id, pairs)` -- typed arguments only, see
  `docs/notes/client-lib.md`. Nothing is written before the answer.
- **settle** applies those pairs on `cmd_result { ok }`, and then SPEC's
  `sticky to xxx`: an entry that follows a model the command changed is forgotten
  unless the command stated it too. `Sink::settled` is how the answer reaches here --
  a `Tui`-side queue the loop drains, so the outcome travels with the frame it came in.

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

## give_back

The window half of `Client::unconfirmed`: the lines a dead link left unanswered go
back into the prompt, newest last, so pressing Enter sends the same words again. A
draft in the prompt is not replaced -- it is the operator's next line, and the
command row already named what was lost while the input history still has it.

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

The address in force is the window's: `connect_server` saves it and the loop dials what
it saved, starting from the entry `client.toml` holds (`docs/notes/tui-state.md`). A
one-shot command has no such memory -- it names its server or reads `GONGGA_SERVER`.

The window and the link are separate things. Nothing here makes the server's
presence a condition for the client to exist: the TUI opens first, dials from
inside the loop, and when the link dies it goes back to dialling instead of taking
the window down with it.

A restart therefore looks like two lines in the log -- `connection lost`, then
`connected` -- with the input line, the history and the scroll position exactly
where they were.

# Invariant

The window renders its own state, and a greeting only corrects it: the session
list is seeded from `roster/<endpoint>.toml` (`docs/notes/tui-roster.md`) before
the first dial, so a start with no server still draws the windows this client last
read. What a greeting cannot reach is the journal, which is why the rows come back
with no link at all, and what it does reach is the list: `set_sessions` drops the
seed, so a session deleted while this window was away stops being drawn at the
moment the server says so rather than staying behind as a local ghost.

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

A burst is also where a hole is seen, and a hole is answered by dialing again. The
bus drops events for a connection that falls behind -- a reader busy painting is
exactly that -- and what it drops is not one session's rows: a deletion is published
as one name, and a name the client never received is a session it believes is still
there. `Client::take_hole` says so once, the link is dropped, and the next dial's
greeting is the whole list again -- the same announcement that emptied a deleted
session's window in the first place (`docs/notes/client-lib.md`). Nothing of the
operator's is lost with it: a line the server has not answered comes back through
`set_offline`, the way it does when a link ends.

The window draws the session a command names, and works it out for itself: before
the command goes out, `Client::show_named_session` reads the catalog's kind and shows
that session. A session the server announces is not shown -- nobody here asked for
it -- so a session somebody else created never takes the events area on its own. See
`docs/notes/client-lib.md`.

The one thing done before the window opens is `cli::home_of`: the loop needs the
home for the server it may start, and nothing else. The command an agent's shell
types is not this front-end's to write -- a shell only exists inside a server, and
the server writes it from its own program as it comes up (`client-server.md`).

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

## declare and answer_transcripts — a refill waits for a frame

`declare` runs once per greeting: for every session the server lists, it asks the
window's own journal where that window's record ends and tells the client. A
window with no journal declares nothing, and an undeclared window is never
fetched — a fresh client has nothing to be behind on.

`answer_transcripts` runs after every step, beside the completion's own drain, and
asks the server only for the sessions **this frame drew**: a session nobody is
reading costs nothing, which is the whole point of a greeting that states stamps
instead of content. The answer arrives as the events that drew those rows live, so
a refilled window is drawn by the code that drew it the first time — no second
renderer, and no shape for a refill to get wrong.

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
