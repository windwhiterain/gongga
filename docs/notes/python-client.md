# `python/gongga.py`

## module doc

The harness is a server that answers commands by name, so a Python program that wants to
talk to it should say `server.set_session(name="main")` rather than build a command line and
parse JSON off stdout. That is the whole module: a handshake, one dictionary in, one `Reply`
out, and a `__getattr__` that puts every published command on the object.

Which names exist is the server's answer. The greeting carries the catalog, and a name that
is not in it raises `AttributeError` *here* -- the same rule the command line follows, where
the client keeps no command list of its own. A typo then costs nothing and says what the
server does publish.

Two shapes, because two callers want different things: `server.call(...)` answers with a
`Reply` whether it was refused or not (a loop that reports its own failures), and
`server.set_session(...)` raises `GonggaError` carrying the server's own words (a script
that stops at the first refusal). The same `Reply` is behind both.

## What the wire actually says

The probe in `scripts/operation_probe.py` got the framing right first, and this module
started from it; four things it did not have to care about cost an hour each:

- **Two envelopes.** A greeting names its kind outside (`{"t": "hello", "v": {...}}`); an
  event names it inside (`{"v": {"t": "cmd_result", ...}}`). A reader that treats one as the
  other sees an empty catalog and a `pid` of zero -- exactly what an unreachable server looks
  like, wrongly.
- **The tag is `command`, not `cmd`.** `cmd` is the *client-side echo* line, not the wire
  variant. A wrong tag is not refused: the server logs `dropping unparsable message` and the
  caller waits until it gives up.
- **A frame carries a sequence and a message id, and a repeat of either is dropped.** Two
  commands sent with the same pair are one command as far as the channel is concerned, so
  both counters are per-send.
- **`ack` is the next sequence expected**, not zero and not the highest number seen. The
  handshake tolerates zero; a command after it does not, and an ack that skipped a gap
  would retire a packet the peer never delivered.

## Acknowledgement, or the connection dies after a second

The channel retransmits what the peer has not acknowledged and gives up on the connection
when it has retransmitted enough -- about a second and a half at loopback timings. A client
that only ever *sends* frames therefore dies exactly when it is waiting: one command in
flight, no reply yet, nothing to piggyback an ack on.

Measured: `check --input 'sleep 3; echo slow'` answered nothing and the caller gave up after
its own 30 s, while the server logged the program's output and `client disconnected` 1.6 s
after the connection opened. `gongga-cli` showed it as `check was sent and never answered`.

So `_whole` acknowledges every data frame it accepts (`_acknowledge`), whether or not the
message it belongs to is complete yet: the sequence numbers are the frame's, not the
message's, and the sender retires a frame, not a message. `_expected` is the next sequence
expected and `_ahead` holds what arrived past a gap, so the ack stays cumulative -- the same
rule `crates/protocol`'s receiver applies when it answers the peer.

## `_event` — three envelopes, not two

An event carries one more layer than the section below describes: the server now
sends `{"t": "event", "v": {"seq": n, "event": {...}}}`, where `seq` is the
server's stream cursor and is absent on an event that is only this connection's
answer. `_event` answers the inner event and the number; a program that wants to
know whether it missed anything compares numbers, and one that does not could not
care.

The session list is a delta now too: `session` upserts one row and `session_gone`
drops a name, so `server.sessions` is maintained rather than replaced.

## call(command, /, **arguments)

Positional-only, because commands have arguments called `name` (`set_session` has one, and
`del_session`), and a `name=` that meant the command would make them unreachable. The name
of the command is the one thing this module will not let an argument shadow.

Arguments travel as the command's own: a string is a string, a list is JSON for an argument
the schema types as a list, and the server decides what it takes. Nothing is coerced here;
the values are the caller's.

## pump

`call` returns on its own `cmd_result`, which is not the only thing on the link: a session
list, a roster, a turn's events arrive beside it. `pump(seconds)` reads for a bounded time
and applies what it finds, so `sessions` can be as fresh as the caller wants without every
`call` paying for a drain. It never waits for a message -- that is `call`'s job, and a pump
that blocked would hang a loop that only wanted a slightly newer view.
