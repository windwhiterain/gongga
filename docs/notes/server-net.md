# server/net.rs

## net

UDP listener, connection demultiplexing, and per-client dispatch.

One socket serves every client. A datagram whose peer is unknown and whose flags
include SYN creates a connection; everything else is routed to the connection
that already owns that address. Each connection then runs two concurrent loops:
inbound commands and outbound event fan-out.

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
