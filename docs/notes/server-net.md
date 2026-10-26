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

## Peer

`conn_id` makes release safe: if a client reconnects from the same address before
the old connection has finished unwinding, the old task must not delete the new
peer's entry.

## serve (accept loop)

`cancel` stops acceptance; the loop below then waits for the turns that are
already running so nothing is cut off mid-tool-call.

## serve (recv error)

Windows reports `WSAECONNRESET` on a UDP socket when a previous send reached a
port that has since closed. It says nothing about the socket's health, so it must
not be treated as a fault -- otherwise every client that exits without saying
goodbye floods the log.

## serve (known peer)

Clone the sender out and drop the lock before awaiting: a slow peer must not
stall the loop that every other peer also goes through.

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
written. So the loop below spins until the count reaches zero, and if a turn ever
fails to do that, the count is what says so: it is printed while we wait rather
than papered over by an exit.

Every ten seconds the wait names the fact that something is still holding the
process open. Observability, not a cutoff.

## handle_connection (commands run elsewhere)

`dispatch` used to be awaited inside the `select!` that also reads this socket and
forwards events, so for as long as one command was busy the connection answered
neither: a pipelined command waited, and so did every event this client was owed,
including the streaming output of a turn it was watching. The read loop now hands
each command to [`run_commands`] and goes straight back to the select.

Unbounded, because there is nowhere to put an excess command. The transport's own
window is what limits how many can be in flight from one peer, and dropping one
would turn into a client waiting on a reply that will never come.

## run_commands

One at a time, in the order they were read. That is not leftover caution: two
commands of one connection are allowed to depend on each other -- `set_session`
then a `send_message` for the session it just configured -- and nothing in
`dispatch` orders them otherwise. Running them side by side would be a change of
meaning, not of speed, so the fix stops at "must not block the other two".

Results carry their own `id`, which is what the client matches on, so a slow
command costs nobody else anything but time.

`CmdResult` is RPC traffic addressed to this connection: sending it directly,
rather than publishing it on the shared bus, avoids both cross-client id
collisions and loss through a lagged broadcast.

A client that hangs up mid-command is not cut off: dropping the sender only closes
the queue for new ones, so the command already running finishes and publishes its
result before this task ends.

## Queued

Named for what it is at the moment it is created, not for what it is doing.

## handle_connection

Authenticate before revealing anything: an unauthenticated peer must not even
learn the command catalog or the session list.

## key_matches

The key travels in cleartext over UDP, so comparing with `==` would let an
attacker time the response to learn how much of a guess is correct.

## key_matches (loop)

Fold in every byte of both strings so the work does not stop at the first
difference. Short reads are padded with a byte that cannot occur in a hex key,
which keeps a wrong-length guess from matching.
