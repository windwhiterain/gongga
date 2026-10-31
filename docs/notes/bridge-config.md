# bridge/config.rs

## config (module doc)

The server's own settings are *not* duplicated here -- `bind` and `key` are read
out of `config.toml` the same way the CLI reads them, so one harness home has one
source of truth. This file only adds what the CLI has no opinion about.

## Bridge::cwd

Empty means "the bridge's own cwd", which is what you want when it runs from the
repo.

## Bridge::allowed

Empty means **nobody**: an agent with a real shell behind it is a root shell with
a chat interface, so this fails closed. The refusal the bridge logs names the id
to add.

## Bridge::model

The model a session this bridge creates runs on, as `<provider>/<model name>`.
Empty means the bridge creates no session and relays only to the ones that exist.

A session states its own model -- the server has no default for one -- so the
creator names it. That makes this key the bridge's one answer to "who pays for a
chat that nobody set up": naming a model here creates the session on that model,
and relaying to a session a person created leaves the model that person chose
alone, because selecting never names one (`ensure_args`).

## Bridge::max_chars

QQ drops bodies it thinks are too long, so the default sits well under its limit.

## Qq::sandbox

Default true: the sandbox needs no review, no published bot and no IP allow-list,
which is the shape of "I want this working tonight".
