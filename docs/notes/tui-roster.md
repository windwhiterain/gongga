# client/window_roster.rs (was tui/roster.rs)

The session list this window drew last, on disk. A-2 moved the source into the
client library: `PipeFrontend::known_list` reads it before a greeting and
`PipeFrontend::send(Layout)` writes it, and neither answer is a terminal fact.
After A-2 `crates/tui/src/roster.rs` held only a one-line re-export; with the
in-process fallback gone (2026-11-10) that file was deleted too.

## Why a client keeps a list at all

A window renders its own state. That is the whole shape of this front-end: the
server sends deltas, the window applies them to what it holds, and the frame draws
what it holds. The surface list is part of that state, so it belongs to the window
-- the server's greeting is the *validation* of it, not its only source.

Without it, a start that never reaches a server draws nothing: the list decides
which windows exist (`Panes::layout` builds one pane per `Layout::shown`), so no
list means no pane, and a journal full of rows nothing reads. See
`docs/notes/tui-cache.md` for the other half of the same argument -- the rows were
already the client's; this is the index that says which rows there are.

## One file per endpoint, named like the journals

`<client-home>/roster/<escaped(endpoint)>.toml`, escaping as `cache.rs::escaped`
spells it, and the endpoint from `client::endpoint_name` -- the address this client
would dial, resolved from its own configuration. Two servers are two lists, and the
answer is known before any server speaks, exactly as it is for the journals.

A directory of its own, not a key inside `client.toml` and not a file beside the
journals: a session name is operator text and may be anything, so the key that
names a file must not also be able to collide with one.

## The shape

```toml
[[session]]
name = "cache"
status = "idle"
title = "42% · 8k ctx · sonnet"
```

One key, so a field added later cannot be mistaken for a row, and one row per
session as the list held it: the name, SPEC's status word, and the pane title the
announcement spelled. Nothing else is recorded -- no `created_at`, no `transcript`
position -- because the file says which names to draw and never how far any
transcript reaches. That is the difference between this and a second source of
truth, and it is why a greeting can simply replace the list.

## What `load` answers, and what it refuses to claim

`status = "error"` is read back as `error`; every other word is read back as
`idle`. A turn was `running` when the window closed and nobody knows how it ended,
so a list drawn before a server answers must not say a turn is running -- that is a
claim the record cannot support. An error is the one word a stale list may still
carry, because it is the one the operator would want to see again.

A missing file, unreadable text or a malformed row is an empty list: the window
then draws only what a greeting or an event says, which is the behaviour of a fresh
client.

## `know` -- written by the frame that drew a list

`PipeFrontend::send` calls `know` for every `RenderMessage::Layout`, which is
once per list the client announces. The write happens only when the rows
changed, so a redraw costs nothing and a list that never changes is never
rewritten.

The whole file, from one place. A partial write would put two writers on one
file, and a window that cannot record its list is not a window that stops
drawing one: the client reports the failure on stderr and still sends the
frame. A `Layout` with no listed sessions is the same as an empty record, so
`know` writes nothing at all.

## The greeting is the validation

`Client::set_sessions` -- the greeting, and only the greeting -- clears the seeded
rows. A name the greeting does not announce is a name the window may no longer
draw, and since the greeting's list is also what replaces the record (through
`know`), the file and the server converge at the first frame after it.

Two more paths prune a seeded row without waiting for a greeting: `apply_session`
(the server has just spoken about that name, so the window's own row is the older
word) and `drop_session` (a session deleted while this window watched).

Until then the rows stand in `Client::listed()`, after the announced ones and never
over one: an announced name wins, because the server is what is and the file is
what was. Everything that reads the list reads that one function -- the frame, the
solo/show filter of `set_render_session`, and the window name `cmd` must not take.

## What this does not buy

Nothing is fetched on a seeded name's behalf: it has no `created_at` and no
`transcript` position, so `declare` never declares it and `behind` never asks about
it. The rows come from the journal, which is this window's own record, and the
refill comparison still starts at the first greeting that announces the name.

Completion does not offer a seeded name either: the catalog of names comes from
`SessionInfo` rows, which the greeting and the stream carry. A start with no server
can read the list and open a window; it cannot complete a name it has never been
told.
