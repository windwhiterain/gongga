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
ship start_fetch  the content fetch, on its own thread. Nobody joins it here.
ship prepare  resolve the content commit from the checkout. Waits for that fetch
              only when the checkout declares no protocol this build can read.
migrate       the db is brought forward before anything opens it.
ship install  reconcile the content into the roots. Local git reads and copies.
prompts       templates from this config, over the pin.
store         the db.
...           shell command, scripts, tools, subscriptions, dispatch, repair, sweep.
bind          the port answers. Everything above is what a client waits for.
--- behind the port ---
agents::start      the community clone's fetch + rebase, on its own thread.
ship::refresh      waits for the start's content fetch; resolves again and
                   reconciles the increment into the roots.
(model scripts)    spawned by the roster read; their models merge in and bump a
                   generation, which is what republishes the roster.
```

Measured on 2026-11-14, live home (`~/.gongga`, 233 MB db, 14 providers), on the
build that started the content fetch entirely behind the port:

| | before | after |
| --- | --- | --- |
| process → `listening` | 10.3 s | 1.8 s |
| content fetch finishes | in front of the port | +4.7 s, behind it |
| community sync finishes | +3.1 s, in front of the port | +4.3 s, behind it |

## The invariant

**A window never waits for a network.** Three things are a network away -- the
content repository, the community clone, and the provider scripts. The content fetch
starts on its own thread as soon as the home is claimed, so the socket is bound
without joining it; the community clone and provider scripts start behind the port.
None of them refuses the start when it fails: an offline machine starts in the same
time as an online one, and says in one warning what it could not refresh.

Two readers do wait, both written down rather than hidden. `ship::resolve` waits for
the fetch already in flight when the checkout declares no protocol this build can
read -- a build whose content moved cannot reach the port otherwise, and waiting for
one fetch is not a second round trip. A home with **no content checkout at all**
clones on that first start, and that clone waits; there is nothing on disk to serve,
so there is nothing to be early for.

## What refresh does

It waits for the fetch this start made, resolves the checkout again and reconciles
the increment into both roots. Content is live tracked (SPEC: `ALL Config File Must
Be Live Tracked`), so a moved checkout is installed now rather than promised to the
next start. `install_at` merges against `.ship-base` and writes only what changed: a
local edit nobody upstream touched is left alone. `ship::content` is a `RwLock`, so
the new commit is the answer for every later reader in this process.

## The other half: the client's poll window

The server being fast is only half of "connected". `crates/client/src/runtime.rs` polls
a server it spawned at 50 ms for `SPAWN_ATTEMPTS` (120) tries -- 6 s -- and then falls
into the reconnect ladder (250 ms doubling to 5 s). A cold start used to take 10 s, so
the window ran out *before* the server was ready and the first successful dial landed
~4 s late, in the ladder. Any start that outlives 6 s pays that again.
