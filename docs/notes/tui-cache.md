# tui/cache.rs

## cache.rs

The window's own copy of what it drew, on disk. A session outlives a terminal: the
server has the transcript, but the *rendered* events -- the folded outline, the
verdicts, the row breaks -- existed only in the client's memory, so a window that
restarted came back empty.

This is the client's answer to that, and it is also how a conversation can run for
days: a block's body is written before it is drawn, and memory keeps only the rows a
pane is drawing.

## One file per window, one writer

`<home>/cache/<window>.journal`, append-only. The window's name is escaped byte by
byte into a file name (`escaped`): it is not a file name -- `cmd ` ends in a space,
and a session may be called anything -- and two names that differ in one character
must not share a journal.

Only one window writes a journal, and the thing it agrees with is the server's
`owner`: a server has exactly one, so a window that is `Ours` or `Adopted` writes and
every other window on that home reads (and never writes). That is why two windows on
one home cannot interleave their records -- the failure would be the same line drawn
twice, and there is no id in the sink vocabulary to deduplicate it by.

The alternative was an OS file lock, and it cost raising the declared MSRV to 1.89
(`File::try_lock`), which is a decision for the whole workspace rather than for this
crate.

## Record — a sealed block stands on its own

`Seal` carries the whole block: id, kind, tag, key, text, whether it is opened, and
the structured call head if it has one. Reading one record is therefore enough to
draw that block, with no earlier record needed, which is what makes a random read
possible at all.

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

`reset` truncates a journal, which is what `Log::clear` means: the command line holds
the last command and no more, so its file should hold that too. `remove` deletes it,
which is what `Log::forget` means: the session it belonged to is gone.

## Cache::set_writable

Until the greeting says whose server this is, nothing is written. The flag is set from
`Tui::set_server`, and it follows ownership across a reconnect: a window that loses
ownership stops writing rather than fighting the window that has it.

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

## A resize is the one thing that reads everything back

Every cached row offset was measured at a width, so a resize invalidates all of them,
including the blocks that were dropped. `lay_out` therefore reads each dropped block
back, counts its rows at the new width and drops it again.

That is the same full pass a resize already paid -- `rebuild` re-laid-out every block
before this existed -- and the only new cost is reading the bytes back. It is the one
place where a very large session can be felt: a shape change on a journal of hundreds
of megabytes reads hundreds of megabytes.

## What this does not do

It does not replace the server's transcript, and it is not a second source of truth:
the rows are what this window was told, and a window that has never been told anything
starts empty however much the server remembers. Showing a session on a *second*
machine is still `get_messages`.
