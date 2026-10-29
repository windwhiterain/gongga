# bridge/main.rs

## main (module doc)

It is a protocol client like `cli` and `tui` -- same key, same handshake, same
event stream -- with a chat platform bolted onto the far side. The server does
not know it exists, which is the whole point: no new command surface, no second
writer to the transcript, no session state invented on the server for the
benefit of a chat app.

The loop is single-threaded on purpose. Two conversations share one state machine
(which chat is owed which answer, and where its transcript cursor stands);
nothing about that gets easier with concurrency. The channel reads in its own
task and hands messages over a queue, so the only await inside the loop is the
platform's send call.

## READ_LIMIT

Rows per read-back. One turn with 20 tool calls is ~45 rows, and 500 is the
server's cap, so this is "the whole turn" without asking for the archive.

## Bridge::accept

Their own id is the one piece of information the refusal has to carry: without
it the operator cannot fill in the allow-list.

## Bridge::pump

A conversation with no known cursor first asks where its transcript ends; until
that answer arrives the message stays parked, because writing to the transcript
first would make the read-back return the whole history and relay an old answer
as a new one.

## Bridge::pump (a stop)

Stopping is a command, not a prompt -- and it must not go near `cancel`, which
shuts the whole server down and takes every other conversation with it.

## Bridge::pump (what is owed after a stop)

The answers are no longer wanted, so stop owing them -- otherwise the next Idle
would read back and relay an answer to a question the user just withdrew.

## Bridge::pump (set_session)

`set_session` doubles as creation and is idempotent, so every message ensures its
session rather than tracking which ones exist.

## Bridge::rows

A read no longer hands back `data.messages`: each row arrives on this connection, as
the event it was published as, before the result. The bridge buffers the `response`
parts sent under `REPLAY_TURN` -- the mark that says a part answers its own read rather
than a turn still streaming -- per session, and takes the buffer when the result for
that read lands. What to relay is decided by the range the read asked for and by
`final_reply`.

The buffer is reset when a read is issued, not when it is answered: a probe reads a row
too, and rows left from a question nobody is asking are not history.

## Bridge::on_idle

Idle also fires for turns this bridge did not start (a resumed turn, or someone
typing in the TUI). Only read back what is owed.

## deliver

A platform that refuses a reply must not take the bridge down: the answer is
still in the transcript, and the next message will work.

## is_stop

The bare words `stop` and `cancel` are not included on purpose: a chat user types
"cancel the last commit" to the agent all the time, and silently turning that
into a stop would be a bug with a very confusing repro.

## main (the socket task)

The socket lives in its own task so the bridge loop only ever waits on a queue --
a reconnect inside the platform code cannot stall the harness side, and vice
versa.

## main (redial)

Reconnecting is the bridge's job, not the platform's calling loop's: the harness
is replaced on purpose (`$GONGGA_HOME/skills/evolve/evolve.sh swap`), while the platform connection
is meant to stay up for days. A bridge that exited with the old server took the
whole chat integration down and needed a human to start it again.

## REDIAL_MIN

Not a budget on how long the bridge keeps trying -- it keeps trying for as long
as the process lives, because the harness may be down for a rebuild -- but a
ceiling on how often it hammers a harness that is not there.
