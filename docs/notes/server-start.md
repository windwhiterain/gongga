# A start, and what is allowed to be in front of the port

`gongga_server::run` does one thing in order, and the order is the whole latency a
window feels: every step before `net::bind_address` is time a client spends dialling a
port nobody is listening on.

## The order, and why each step is where it is

```
config        config.toml read; the roster file read. No script is joined here --
              unless the file cannot place the model, and then it is, on purpose:
              a home whose default model only a script names has no other answer.
home lock     one server per home, before the db is opened.
ship prepare  resolve the content commit from the checkout. No fetch (see below).
migrate       the db is brought forward before anything opens it.
ship install  reconcile the content into the roots. Local git reads and copies.
prompts       templates from this config, over the pin.
store         the db.
...           shell command, scripts, tools, subscriptions, dispatch, repair, sweep.
bind          the port answers. Everything above is what a client waits for.
--- behind the port ---
agents::start      the community clone's fetch + rebase, on its own thread.
ship::refresh      the content fetch; names the commit the next start installs.
(model scripts)    spawned by the roster read; their models merge in and bump a
                   generation, which is what republishes the roster.
```

Measured on 2026-11-14, live home (`~/.gongga`, 233 MB db, 14 providers):

| | before | after |
| --- | --- | --- |
| process → `listening` | 10.3 s | 1.8 s |
| content fetch finishes | in front of the port | +4.7 s, behind it |
| community sync finishes | +3.1 s, in front of the port | +4.3 s, behind it |

## The invariant

**A network is never in front of the port.** Three things are a network away --
the content repository, the community clone, and the provider scripts -- and none of
them is what a window waits for. Each is started after the socket answers, and none of
them refuses the start when it fails: an offline machine starts in the same time as an
online one, and says in one warning what it could not refresh.

The one exception is written down rather than hidden: a home with **no content
checkout at all** clones on that first start, and that clone waits. There is nothing
on disk to serve, so there is nothing to be early for.

## What refresh must not do

It answers with a commit and installs nothing. A start's install is the commit every
reader in that start already agreed on (`ship::content`); rewriting skills under a live
turn is a file a tool may be reading, and a transcript that disagrees with the roots it
was built against. So the fetched commit is a name for the **next** start, logged when
it differs.

## The other half: the client's poll window

The server being fast is only half of "connected". `crates/client/src/runtime.rs` polls
a server it spawned at 50 ms for `SPAWN_ATTEMPTS` (120) tries -- 6 s -- and then falls
into the reconnect ladder (250 ms doubling to 5 s). A cold start used to take 10 s, so
the window ran out *before* the server was ready and the first successful dial landed
~4 s late, in the ladder. Any start that outlives 6 s pays that again.
