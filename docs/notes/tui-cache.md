# tui/cache.rs

## cache.rs

The window's own record of the rows it was given, on disk.

A client does not have the server's transcript, and does not want it: what it has is
the events it was told about, in the order it was told, with the folds and the row
breaks the operator was reading. That is a client fact, so it lives in a client
directory and nothing here asks a server where it should go.

Two things follow. A window that restarts comes back with what it had been reading.
And a session can run for days: a block's body is written before it is drawn, and
memory keeps only the rows a pane is drawing.

## The client's own directory

`--client-home`, then `GONGGA_CLIENT_HOME`, then `~/.gongga-client`
(`cli::client_home_of`). Never `--home`: that one is the server's, it holds
`config.toml`, `gongga.db` and the blobs, and on the machine the operator is sitting
at it may not exist at all.

Under it, `events/<endpoint>/<window>.<pid>.journal`. The endpoint is
`cli::endpoint_name` -- the address this client would dial, from its own configuration
-- so two servers are two sets of records, and the answer is known before any server
speaks. Nothing here is derived from a greeting: a greeting arrives after the window
has already decided where its rows go.

## One writer, and the writer is a process

The pid is in the file name. Consequence: two windows on one machine never append to
the same file, because they are two pids; and a restart takes over the file the dead
process left, because `cli::server::alive` says it is dead.

`Cache::claim` runs once, at construction, over the directory: for each window, keep
the file if it is already mine; otherwise, if the newest file's writer is gone, rename
it to me (which keeps a session at one file however often the window was opened);
otherwise start a file of my own -- another window is writing this session, and its
records are its own. Files left by pids that are gone and were not chosen are deleted.

A lost rename is not an error. Rename is atomic, so of two windows starting at the
same moment exactly one takes the file over and the other starts fresh: losing some
history, never interleaving two writers.

This is the client-local answer to "who may write": it asks the machine, and the
machine is what both windows are already standing on. It replaced a rule that asked
the *server* who owned it -- which meant the window that had not started the server
wrote nothing at all, and that is exactly the case of a server on another machine.

## Record — a sealed block stands on its own

`Seal` carries the whole block: id, kind, tag, key, text, whether it is opened, and
the structured call head if it has one. Reading one record is enough to draw that
block, with no earlier record needed, which is what makes a random read possible.

`Delta` is one more piece of a block that is still growing. It is *not* self-sufficient
-- that is the whole difference -- so a block with deltas and no seal can never be
dropped, and the newest page of a journal, which is where an interrupted stream lives,
is rebuilt by replaying forward from a record boundary.

Nothing else is recorded. `rewrite`, `relabel`, `describe` and a toggle all append a
fresh `Seal`, because each of them changes the block's form and the newest seal wins.

## Framing — `[u32 len][version + json][u32 len]`

The trailing copy of the length is what makes a walk *backwards* possible: a record's
length is otherwise only knowable from its front, so reading the newest page would
mean walking the whole file forwards.

The version is the first four bytes of the body, so a build that meets a journal it
does not know stops there rather than misreading a record. `FRAME` is that length
field, both times.

## Cache::append, Cache::read, Cache::back, Cache::forward

`append` answers with the offset the record starts at; a `Seal`'s offset becomes the
block's `slot`, which is how the body is found again. `read` is one record at an
offset. `back` walks backwards over record boundaries and answers where the `count`-th
previous record begins, and `forward` reads from there to the end -- the two together
are what a seed is built from.

`reset` truncates this process's file, which is what `Log::clear` means: the command
line holds the last command and no more, so its file should hold that too. It also
deletes every abandoned file of that window; a file another live client is writing is
left alone, because it is that client's record. `remove` is the same statement for a
deleted session.

## Cache::none

A `Log` built with no front-end in mind keeps nothing, and derives no path from an
empty root: `Cache { dir: None }` answers "no record" rather than creating a directory
in whatever directory the process happens to stand in.

## log.rs — Entry::slot and Entry::cold

`slot` is where the journal holds this block's whole current form; `cold` is that
block's row count when its body is not here. They are the only two fields the cache
added to a block, and they are what makes dropping a body invisible: the row index
still counts it, so scroll bounds, click hit-testing and `Pane::anchor` do not move.

`rows()` answers from `cold` first, `reflow` refuses to lay out a body that is not
here, and `hot()` is the question the rest of the file asks before touching `text`.

## Log::seed — a window that has never drawn starts from its journal

`seed(window, width, rows)` is called by the frame that is about to draw a window it
has no blocks for. It reads backwards in `WALK`-record batches, replays what it found,
and stops as soon as the window covers `rows` -- the pane's own height.

Two things follow. The cost of opening a session is a page, not a transcript: the
biggest session on this machine (1602 events, 2.5 MB of text) opens having replayed 73
rows. And a window that has blocks is left alone: they are here because this process
watched them arrive, and replaying on top would draw every row twice.

`replay` drops the window's blocks, applies the records in order, and then seals
everything still open. Nothing survives a restart as an open stream -- the process
that was writing it is gone -- so `Delta` records whose `done` never arrived are the
tail of a block, not a block that is still growing.

## materialize — the body comes back at the width it is drawn at

Rows are a function of the text *and* the width, so the read-back takes a width and
lays the block out at it. That is what makes the count the index kept still correct:
the same text at the same width has the same rows.

A record that cannot be read leaves the block as it is rather than inventing an empty
one. A journal truncated by hand is then a hole in the picture, not a lie.

## Log::touch — a mutation reads the body back first

Every mutation (`rewrite`, `relabel`, `describe`, a toggle, an append) calls `touch`
before it changes anything. The reason is the record: it is dumped from the entry, and
an entry holding only rows would write an empty block over a block the operator can
see. That is the one way this cache could destroy what it is supposed to keep.

## Log::evict — what a frame draws is what memory keeps

After a pane has drawn, `Log::evict(window, start..end)` drops the bodies of the blocks
that are entirely outside those rows: the blocks scrolled past, and the ones below the
view. `Panes::begin_frame`/`end_frame` complete the rule for a window nobody drew at
all -- a hidden session holds no bodies.

There is no budget and no number to tune. The bound is the render: a window costs the
rows it is drawing, plus whatever the operator scrolled past without coming back to the
bottom. `hot` is the list eviction walks, so the pass costs what is *loaded*, not what
the session has ever said.

## Log::repoint — another endpoint is another record

`connect_server` moves the address, so `Tui::repoint` builds a cache for the new one
and drops every window's blocks: the rows on screen belonged to the server that is no
longer being dialled, and the next frame seeds each pane from the new endpoint's
records. Nothing is deleted -- switching back finds the old ones.

## A resize is the one thing that reads everything back

Every cached row offset was measured at a width, so a resize invalidates all of them,
including the blocks that were dropped. `lay_out` therefore reads each dropped block
back, counts its rows at the new width and drops it again.

That is the same full pass a resize already paid -- `rebuild` re-laid-out every block
before this existed -- and the only new cost is reading the bytes back. It is the one
place where a very large journal can be felt: a shape change reads all of it.

## What this is not

It is not a mirror of the transcript, and it does not try to be one. A window records
what it was told; if it was closed while the server kept working, the rows from that
time are simply not in it, and nothing invents them. The protocol has no "send me what
I missed" that would not make a front-end into a second reader of the transcript, and
`get_messages` is the command for an operator who wants that.

Showing a session on another machine is therefore also `get_messages`: the records of
one client are that client's, and a second machine keeps its own.
