# bridge/channel/mod.rs

## channel (module doc)

Two traits instead of one struct, mirroring how `protocol` splits a connection
into a sink and a stream: an inbound half that owns the socket it reads events
from, and an outbound half that is just an API client and can be called from
anywhere. Keeping them separate is not cosmetic -- a single `&mut self` type with
both `recv` and `send` cannot be driven from one `tokio::select!`, because both
futures would borrow it at once.

Everything platform-specific (tokens, websocket framing, how a reply is attached
to its source message, length limits) lives behind these traits, so `main.rs` and
`chat.rs` never learn which IM is on the other end.

## ChatId::session_name

Deterministic on purpose: there is no mapping table to lose, and a restart picks
every conversation up exactly where it left off. The platform id is already
unique, so the derived name is too.

## Incoming::reply

Some platforms refuse a reply that does not reference it (QQ: five minutes, five
replies), so it travels with the message rather than being looked up later.

## Outgoing::reply_to

`seq` counts the attempts on it, because platforms cap how many replies one
message may carry (QQ: 5).

## Inbox::recv

Implementations reconnect internally; an error means "retry after a pause", not
"give up".
