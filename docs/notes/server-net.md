# server/net.rs

## net

UDP listener, connection demultiplexing, and per-client dispatch.

One socket serves every client. A datagram whose peer is unknown and whose flags
include SYN creates a connection; everything else is routed to the connection
that already owns that address. From then on a connection is three things at once:
it reads commands, it forwards events, and it runs the commands it has accepted --
the last of those off to the side, so that neither of the other two waits for it.

Command echoes and results are RPC on their requesting connection. The event bus
is reserved for genuinely shared response, tool, status, and log events.

## The greeting's cursor, and the bus gate

A greeting states the stream position this connection starts from, so a client
knows the first number it should see. Every event after it is numbered above, and
a number that is not the one after the last is a hole.

A lazily fetched transcript is read inside `EventBus::as_of`: the rows, the parts
still streaming and the number are all taken with publishing stopped, so the answer
is a picture of one instant rather than of two. The same gate is what
`EventBus::step` uses to keep an open answer's text and its stream number together.

`as_of` runs its closure under the gate, so it must not publish — and what it reads
must not hold a lock a publisher takes, or the two would wait for each other.

## Peer

`conn_id` makes release safe: if a client reconnects from the same address before
the old connection has finished unwinding, the old task must not delete the new
peer's entry.

## serve (accept loop)

`cancel` stops *acceptance* -- `Dispatch::shutting_down` refuses to start another
turn -- and nothing else. The socket stays open and the loop keeps answering while
the turns that are already running finish, because the drain can take minutes and a
window attached to this server has to be able to ask what is happening. It used to
break out of the loop on the shutdown token, which left the socket bound but unread:
every datagram a window sent went nowhere, so the window saw silence, called it a
disconnection, and printed "reconnecting" for the whole of a cancel that was in fact
going fine.

## serve (recv error)

Windows reports `WSAECONNRESET` on a UDP socket when a previous send reached a
port that has since closed. It says nothing about the socket's health, so it must
not be treated as a fault -- otherwise every client that exits without saying
goodbye floods the log.

## serve (known peer)

Clone the sender out and drop the lock before awaiting: a slow peer must not
stall the loop that every other peer also goes through.

Every lock on `conns` takes a poisoned guard as its data
(`unwrap_or_else(|e| e.into_inner())`), which is what the rest of this server
already does. This map is the address table the accept loop reads on every
datagram: an `unwrap` here would let one panic anywhere in the process become
the network thread dying as well, and every client would then see the exact
silence `catch_unwind` was added to the turn for.

## serve (stale sender)

The connection task is on its way out and will release the entry itself. Only
evict if the address still maps to *this* sender: a reconnect from the same
address may have replaced it.

## serve (connection task)

A peer leaves in three ways: `Bye`, a failed send, or the client vanishing. The
first two are noticed elsewhere; the third is not -- and a client that reconnects
from a fresh local port leaves its old address behind forever. Release it here,
unless the same address has already been re-inserted for a newer connection.

## serve (shutdown)

There used to be a 30s deadline here, and it was a budget like any other: it
decided that a turn had had long enough, and a turn still committing a tool
result when it expired lost that work. Nothing needs the deadline either,
because the wait has a real end -- `cancel` hands every running turn the same
token, and a turn checks it at every point where it waits (model stream, tool
call, retry delay) and leaves at its step boundary with its rows already
written. So the loop waits until the count reaches zero, one [`DRAIN_TICK`] at a time, and
if a turn ever fails to do that the process simply stays up, still answering --
which is a state a reader can see, rather than an exit that hides it.

The wait is inside the accept loop, not after it, and that is the whole point: a
draining server still serves. `cancel` records a `server_stop` row, and publishing
it is what tells every attached window that the server is stopping -- and who asked.
`ServerInfo::draining` says the same to a window that dials mid-drain, and it is read
from `shutting_down` rather than from the socket's shutdown token, so the notice
window before the token is cancelled counts as draining too.

## handle_connection (commands run elsewhere)

`dispatch` used to be awaited inside the `select!` that also reads this socket and
forwards events, so for as long as one command was busy the connection answered
neither: a pipelined command waited, and so did every event this client was owed,
including the streaming output of a turn it was watching. The read loop now hands
each command to [`run_commands`] and goes straight back to the select.

Unbounded, because there is nowhere to put an excess command. The transport's own
window is what limits how many can be in flight from one peer, and dropping one
would turn into a client waiting on a reply that will never come.

## handle_connection (operations)

An operation arrives beside the commands and is answered where a command is:
`Operation::as_command` turns the one the server owns into the command it
mirrors, and the same queue, the same dispatch and the same answer carry it.
The two the client owns are refused by name -- they are the caller's own state,
and a server that changed them would change the wrong side of the wire.

## a client that is not reading

Two lines name it, both at warning level because both mean events were lost or
late: `client fell behind` when the client's own subscription skipped events, and
`client is not reading` when queueing one event took longer than a quarter second.
A client that stalls its reads collapses the link's window, and because the
channel is ordered, everything behind it waits -- including the reply to a
command the operator just typed. Silence there used to look like a slow agent.

## run_commands

One at a time, in the order they were read. That is not leftover caution: two
commands of one connection are allowed to depend on each other -- `set_session`
then a `send_message` for the session it just configured -- and nothing in
`dispatch` orders them otherwise. Running them side by side would be a change of
meaning, not of speed, so the fix stops at "must not block the other two".

Results carry their own `id`, which is what the client matches on, so a slow
command costs nobody else anything but time.

Each command is logged as it runs, with two durations and a session: `waited` is
how long the datagram sat behind earlier commands on the same connection, `took`
is the dispatch itself. A one-line `send_message` is therefore either "accepted:
the turn starts now" or "queued: a running turn reads it at its next step
boundary", and the boundary delivery logs the age of the message it delivered.
Those three lines are what tells a slow answer apart from a slow link.

`CmdResult` is RPC traffic addressed to this connection: sending it directly,
rather than publishing it on the shared bus, avoids both cross-client id
collisions and loss through a lagged broadcast.

A client that hangs up mid-command is not cut off: dropping the sender only closes
the queue for new ones, so the command already running finishes and publishes its
result before this task ends.

## Queued

Named for what it is at the moment it is created, not for what it is doing.

`caller` is the session the connection was started from, copied off the greeting so
that every command on it is run as that session -- once per connection, not once per
command, because who dialled is not a per-command question.

## authenticate

It answers with the `ClientInfo` it read, not with a boolean: the client's own account
of itself is what the commands on this connection are run as, and the greeting is the
only place it is ever said.

## handle_connection

Authenticate before revealing anything: an unauthenticated peer must not even
learn the command catalog or the session list.

## the greeting, then the last reading

Hello is the sessions, the catalog and the server's facts; the allowance is not
part of it, because it is a measurement rather than a description of the server,
and it is one value for the whole process rather than a field of anything Hello
already carries.

A window that attaches between two steps is told the cached reading straight
after the greeting, to its own connection only. Without it the row would stay
blank until the session happened to spend something, which reads as a feature
that is not working. A broadcast would have reached every other window with a
value it already has.

## key_matches

The key travels in cleartext over UDP, so comparing with `==` would let an
attacker time the response to learn how much of a guess is correct.

## key_matches (loop)

Fold in every byte of both strings so the work does not stop at the first
difference. Short reads are padded with a byte that cannot occur in a hex key,
which keeps a wrong-length guess from matching.
