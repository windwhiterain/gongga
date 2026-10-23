# protocol/channel.rs

## channel

UDP gives us no ordering, no delivery guarantee and no fragmentation, so this
module adds exactly those three things and nothing else: no encryption, no
congestion-control research project, no stream abstraction. The unit of
retransmission is the *packet*; the unit of delivery to the caller is the
*message*, reassembled and emitted in the order it was sent.

Recovery strategy:

* cumulative ACKs, piggybacked on outgoing data whenever possible,
* per-packet retransmission timers with RFC 6298 RTO and exponential backoff,
* fast retransmit of the oldest in-flight packet on the third duplicate ACK,
* a fixed send window intersected with the peer's advertised receive window,
* a bare ACK every `keepalive` interval, so cross-machine NAT mappings do not
  silently expire.

Packets are delivered to the reassembly layer strictly in sequence order
(out-of-order packets are buffered, not delivered), so a message's fragments
always arrive contiguously and reassembly is a simple append.

## ChannelConfig

The defaults are sized for a LAN/Internet link between a single CLI and a single
server, where round trips are a few milliseconds and messages are at most a few
hundred kilobytes.

## Inbound

Two things must not live in the I/O task: how far behind the caller is, and the
reader task that feeds the socket.

## Inbound::pending

This is what the peer is told the receive window is. A consumer that stops
reading therefore slows the sender down instead of filling memory, and --
because nothing here is ever awaited by the I/O task -- without stopping it from
answering ACKs or retransmitting.

## ReaderAbort

The reader spends its life blocked in `recv_from`, and nothing wakes it when its
connection is replaced: the peer stops sending, so its only exit -- a failed
`send` on a closed queue -- never happens. A client that reconnects would leave
one task and one `Arc<UdpSocket>` behind per attempt, which is exactly what a
TUI redialing a restarted server does.

## Channel

The channel owns its socket-side task. When that task exits, `send` fails and
`recv` yields `None`, so callers do not need a separate liveness check.

Dropping every handle (both halves, after `Channel::split`) also stops the reader
task, so a replaced connection leaves nothing running.

## Channel::split

Both halves can be driven concurrently by a `select!` loop, and dropping either
ends the channel. The reader task lives until the last half is dropped, so a half
that is still in use keeps the socket open.

## ChannelSink::_shared

A sink with nobody receiving beside it is still a link somebody may come back to.

## ChannelCore::packet_rto

Backing off per packet rather than per channel is what keeps a burst of losses
from stalling every other in-flight packet: one unlucky fragment does not push
the whole window's timers out.

## Sender

The sending half is immutable and shared with the timer path; the receiving half
is a per-peer inbox that a demultiplexer fills. That split lets a server serve
many connections from one bound port, and keeps `select!` from having to hold a
mutable borrow across branches.

## accept

`inbox` is fed by the server's demultiplexer, which owns the listening socket.

## spawn_inner

Delivery to the caller never goes through a queue that can block: the task below
also drives ACKs and retransmission, and waiting on a slow consumer there stops
both -- the link looks hung while both ends are alive. Ordering and reliability
are preserved instead by *advertising* a smaller receive window as the backlog
grows, so the peer stops sending rather than our memory or our timer loop
growing.

## spawn_inner (advertised window)

What the peer is told about our receive buffer, recomputed every pass: it is
`cfg.window` minus the messages the caller has not taken yet, so a consumer that
stops reading makes the sender slow down instead of stalling ACKs and
retransmits here.

## on_frame

`RST` is an abort: the channel ends with `Reset`. `FIN` is a close: the peer has
finished, so the reader stops at once with `Closed` and the socket is released,
rather than retrying against a connection that is already gone. Both are sticky:
the state moves to `Closing` and nothing is sent after it.

There is no half-close. `FIN` is emitted only when the sender's own reader has
already finished, so its socket is about to go and nothing can still arrive from
that side; the receiver ends both directions rather than waiting to see whether
the sender had more to say. A peer that wanted TCP's "I am done writing, keep
sending to me" would need a different frame, and no such peer exists here.

## on_tick (retransmit)

Retransmit every in-flight packet whose own timer has expired, a handful per
tick. Doing one at a time would turn a burst of losses into a multi-second stall
on a link with any real latency.

## the_advertised_window_shrinks_with_the_backlog_but_never_closes

Zero would be a stall the peer may honour forever; the point is to slow a
producer down while this side keeps acknowledging and retransmitting.
