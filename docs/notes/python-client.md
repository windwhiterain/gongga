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
- **`ack` is the peer's last sequence**, not zero. The handshake tolerates zero; a command
  after it does not.

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
