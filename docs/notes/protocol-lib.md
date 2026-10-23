# protocol/lib.rs

## lib

The crate is deliberately dependency-light so the CLI does not inherit the
server's runtime. The wire layers:

* `frame` — the fixed 26-byte datagram header.
* `channel` — a reliable, ordered, fragmented message channel over UDP.
* `message` — the JSON event vocabulary the two peers speak.
