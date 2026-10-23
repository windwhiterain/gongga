# protocol/frame.rs

## frame

Wire frame for the gongga reliable-UDP transport.

Every UDP datagram carries a fixed 26-byte header followed by a JSON payload
fragment. The header is intentionally small and fixed-size so the ARQ layer can
retransmit payloads verbatim without re-encoding.

```text
 0 ..  2  magic      u16   0x4747 ("GG")
 2 ..  3  version    u8    1
 3 ..  4  flags      u8    SYN | ACK | FIN | RST | DATA
 4 ..  8  conn_id    u32   connection identifier assigned by the server
 8 .. 12  seq        u32   packet sequence number (ARQ unit)
12 .. 16  ack        u32   cumulative ack: next seq the sender expects
16 .. 20  msg_id     u32   logical message a DATA fragment belongs to
20 .. 22  frag_idx   u16   index of this fragment within the message
22 .. 24  frag_cnt   u16   total fragments in the message
24 .. 26  window     u16   receive window, in packets
```

`SYN`, `SYN|ACK`, `ACK`, `DATA` and `FIN` are emitted. `FIN` means the sender is
finished, and the reader acts on it instead of waiting out `idle_timeout`. `RST`
aborts a connection and is understood but never sent here. There is no `PING`:
keepalive is a bare `ACK`, so the flag had no meaning.

## DEFAULT_MTU

1200 keeps a full frame under the Internet safe MTU so cross-machine links never
rely on IP fragmentation.
