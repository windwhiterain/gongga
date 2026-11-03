# crates/server/src/transcript.rs

## transcript.rs

A walk over one session's sendable rows, on a connection of its own. The store
holds the shared connection; a request reads from this one while it is writing
to the network, and the shared connection has to stay free for the turn waiting
on it.

What comes back is the stored item, byte for byte. The request's shape was
decided when the row was appended, so a walk cannot disagree with an earlier
request about what a row is: rewriting a row would rewrite bytes the provider
had already cached, and each such rewrite cost the whole transcript.

`upto_seq` is fixed when the walk is made, so every row it returns was stored
before the request began. That is what lets a retry replay one window: the
window is the same set of rows, not a fresh reading of a transcript that moved
underneath it.

The pictures ride in the row's `display`, which is what the row recorded when it
was written. An item names a copy, and the copy is read when a request is built
-- the provider's file is resolved then, and never stored in the transcript.
