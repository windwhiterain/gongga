# protocol/loopback.rs

## loopback

`#[cfg(test)]` inside the crate, not `tests/`: a `tests/` file is a second test
binary, and it exists to link this whole stack a second time to run proofs that
need nothing the crate does not already have. Nothing here reaches inside the
crate, so what would have been an integration test is a module.

These tests drive the transport the way the server does: one socket, a
demultiplexer that hands frames to per-peer connections, and a client that opens a
connection with SYN. The lossy variant deliberately drops datagrams to prove the
retransmission path actually recovers rather than just working when the network
is perfect.

## dropping_a_channel_releases_its_reader_task_and_socket

A client that reconnects (the TUI redialing a restarted server) would otherwise
leak one reader task and one socket per attempt.

## a_paused_consumer_does_not_stall_the_link

Delivery used to be an `await` on a 256-message queue inside the task that also
answers ACKs and retransmits, so a pause on the receiving side stalled the whole
link: the peer saw no acknowledgements, nothing was retransmitted, and both
processes were alive the whole time. The replies below queue up while nobody
reads, and every one of them still has to arrive, in order.

## a_paused_consumer_does_not_stall_the_link (producer)

Dropping a half closes the channel *by contract*, so a task that returned right
after sending its last message would be closing the link -- the peer hanging up,
not the consumer pausing, which is the thing under test.
