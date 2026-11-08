# client/window_cache.rs (was tui/cache.rs)

## cache.rs

As of 2026-11-08 the source lives in `crates/client/src/window_cache.rs`. The
note keeps its `tui-cache.md` name because it describes the window's journal,
not a crate. The journal's data types moved; the ratatui folding in `log.rs`
stayed in the terminal crate.

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

Asking is the expensive half: `alive` starts a process on Windows. `Ask` answers once
per pid for one walk of the directory, so a claim that finds five files of one dead
window asks the machine once, not five times.

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

## One handle per window, for the life of the process

`Cache` keeps the journal open: `Slot { path, file }` per window, opened on first use
and held until the process ends. The numbers are the reason. On the machine this was
measured on, an open costs about ten milliseconds and a write on an open handle about
three microseconds -- three thousand times less -- so a journal reopened per record
turned every stream into a stall.

The handle is also why reads and writes share one lock: a seek and the transfer are
one step, and no two windows can interleave inside a record.

Two operations cannot happen with the handle open, and both close it first: `reset`
truncates, `remove` deletes. Windows refuses to delete a file a process still holds,
and a truncate behind an open handle is how a write would land at the wrong offset.
The handle comes back on the next record.

## Record — a sealed block stands on its own

`Seal` carries the whole block: id, kind, tag, key, text, whether it is opened, and
the structured call head if it has one. Reading one record is enough to draw that
block, with no earlier record needed, which is what makes a random read possible.

`Delta` is one more piece of a block that is still growing, and only an older build
writes one. It is *not* self-sufficient -- that is the whole difference -- so it never
became a `slot`. It stays in the vocabulary because the journals already on disk are
full of them, and a record a build cannot name is a record it cannot read past.

Nothing else is recorded. `rewrite`, `relabel`, `describe` and a toggle all append a
fresh `Seal`, because each of them changes the block's form and the newest seal wins.

## Record::Stamp and Record::Rewind — where the transcript ended, and what to drop

A `Stamp` is not a block: it says this window has drawn every row of its session
up to `seq`, and which session that is (`created`, so a name handed out again is
not mistaken for the same transcript). It is the client's half of the sync stamp —
the server's half is `SessionInfo::transcript` — and it is the only thing that can
answer "was this window away while rows were written" without shipping rows.

A `Rewind` says everything recorded above the newest `Stamp` is thrown away,
because the server is about to send that tail again. The replay honours it on the
way forward: `Log::cut_tail` keeps the blocks below the stamp and drops the rest,
including the half-written block that no row accounts for.

Both changed the journal's shape, so `VERSION` is 2: an older build cannot name a
`Stamp` record and, by the framing's own rule, cannot read past it. Journals from
before this are therefore not read — a window's scrollback from an older build is
lost once, and what it draws from then on is stamped.

## Record::Note and Record::Clear — what the window said, and where its view starts

A `Note` is not a block either: it is what the window said about itself — a sticky
entry it left off a line, a paste that failed, a frame that could not be painted —
with the kind the status line showed it in. `Tui::say` writes one before the status
line takes the words, so the journal holds every note and the row keeps only the
newest. A replay draws nothing for it: what a window *said* and what it *showed*
are two records, and only the second one is a block.

A `Clear` says the window's view holds nothing above this record. `Log::clear` used
to empty the window's file instead — which made "the view forgot it" and "the record
never existed" the same act, so a command row, a `/set_sticky` answer or a session
that was reseeded took its own history with it. Now the file only grows: the view
drops its blocks in memory, the `Clear` marks the cut, and a replay applies it in
place — everything the window drew before it is read and then dropped, so the rows a
window shows after a restart are the rows it showed before one.

Neither is a block, so both ride the framing the journal already carries and
`VERSION` stays 2: bumping it would make every journal already on disk unreadable,
which is a larger loss than a build older than this one stopping at the first record
it cannot name — it loses the scrollback above that record and nothing below it.

## A stream is written once, not once per chunk

A block that is still growing keeps its text in memory and lands in the journal when
it ends: one `Seal`, holding the whole thing. What that replaced was a `Delta` per
chunk -- a session on this machine had 4514 of them for a single turn -- and each one
was a file operation on the thread that draws.

`Entry::pending` is the state that makes this safe. An append sets it; the seal that
writes the block clears it. A block whose record is older than what is on screen may
not be dropped, because reading it back would lose the newest rows. So a stream costs
memory -- the one block being written -- and the journal never holds a half-arrived
block.

The cost of losing the process mid-turn is that turn's block: it was never written.
That is the trade the journal makes everywhere else already -- it records what a
window was told, not what a server still has.

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
are what a seed is built from. `end` is the file's length, asked of the metadata rather
than of an open handle: a frame asks it for every window it draws, and a window that
has written nothing yet has no file.

`reset` truncates this process's file, which is what `Log::clear` means: the command
line holds the last command and no more, so its file should hold that too. It also
deletes every abandoned file of that window; a file another live client is writing is
left alone, because it is that client's record. `remove` is the same statement for a
deleted session.

## Cache::none

A `Log` built with no front-end in mind keeps nothing, and derives no path from an
empty root: `Cache { dir: None }` answers "no record" rather than creating a directory
in whatever directory the process happens to stand in.

## log.rs — Entry::slot, Entry::cold and Entry::pending

`slot` is where the journal holds this block's whole current form; `cold` is that
block's row count when its body is not here. `slot` and `cold` are what makes dropping
a body invisible: the row index still counts it, so scroll bounds, click hit-testing
and `Pane::anchor` do not move.

`pending` is the third: text this block holds that its journal does not. It is set by
an append and cleared by the seal that writes the block, and it is what `drop_body`
refuses. A block with pending text has a `slot`, because it was sealed once -- the
label, the first chunk -- but that record is older than the screen.

`rows()` answers from `cold` first, `reflow` refuses to lay out a body that is not
here, and `hot()` is the question the rest of the file asks before touching `text`.

## Log::seed — a window whose beginning this process has not decided

`seed(window, width, rows)` is called by the frame that is about to draw a window it
has never decided the start of. The test is `Log::seeded` -- has *this process*
answered where that window begins -- and not "does the window have blocks". It finds
the newest page boundary (`Floor`, see below) and replays one page from it, which is
what the pane draws; a journal written before pages were recorded has no boundary, and
is then read backwards in `WALK`-record batches until it covers `rows`.

That distinction is a bug this replaced. The test used to be "does this window have
blocks", and a restart receives a `ctx … model …` line from the server before its
first frame draws -- so the window looked like one this process had been watching all
along, and the journal was never replayed. A session with thousands of rows came back
as that one line.

`replay` keeps what arrived live: it takes the window's blocks out, applies the
journal's records, and puts the live blocks back after them, replacing any block the
journal also holds by id. The rows are history first, then what this process saw.

The cost of opening a session is a page, not a transcript: the biggest session on this
machine (1602 events, 2.5 MB of text) opens having replayed 73 rows, and a window that
paged through thousands of rows in an earlier session reads back one page of them.

Since a stream lands once, one record is one block: a batch of `WALK` records is a
batch of blocks, and the row count it can cover is no longer a guess.

Nothing survives a restart as an open stream -- the process that was writing it is
gone -- so a `Delta` whose `done` never arrived (an older build's journal) is the tail
of a block, not a block that is still growing.

## Pages — what one fetch landed, and where the journal says it begins

A window is filled a page at a time: `get_messages --replay` with the range a pane can
draw. The client asks for the tail when a window holds nothing, for `drawn + 1 …` when
it is behind, and for `floor - room … floor - 1` when the operator scrolls above the
oldest row it holds. One request per keystroke, never a transcript.

`Record::Floor { seq }` is that page's left edge: the blocks *after* the record are the
transcript's rows from `seq` up, and it is written in front of the block the page began
with. `landed` is what writes it, and because the journal's order is the order the pane
draws, it rewrites the window's records in that order -- a page of older rows goes in
front of the held ones, not behind them. A block whose record is a boundary's worth of
records away keeps its `slot` from the same list, which is why the offsets are read off
the list as it is built rather than counted from either end.

The window, not the journal, says how far back it reaches: `Log::floor_of` is
`ledger.pages.first()`, the oldest page *in memory*. Before the first frame that is the
page a seed is about to replay -- the journal's newest boundary -- so a greeting can
declare it, and a page of the past lands where it belongs.

A window holding rows it cannot name -- a journal no boundary was written into -- is
refilled as a whole (`Sink::forget`) instead of by a page appended behind them, which
would land on rows it already has.

## materialize — the body comes back at the width it is drawn at

Rows are a function of the text *and* the width, so the read-back takes a width and
lays the block out at it. That is what makes the count the index kept still correct:
the same text at the same width has the same rows.

A record that cannot be read leaves the block as it is rather than inventing an empty
one. A journal truncated by hand is then a hole in the picture, not a lie.

Two records stand for a block alone: the `Line` a whole row was pushed as, and the
`Seal` a stream ends with. Reading back used to accept only the `Seal`, and `drop_body`
dropped anything with a slot -- so a command line, a user message or a status row that
scrolled out of the pane, or that belonged to a hidden session, came back as its mark
and nothing else, then as nothing at all. What the operator saw was a pane full of
blank rows with the odd `$` in it, which is the shape of a body that is gone: `row()`
answers `""` from an empty `starts`. A `Delta` is still refused, because it is the tail
of a block and restoring it alone would invent a block without its head.

## Log::touch — a mutation reads the body back first

Every mutation (`rewrite`, `relabel`, `describe`, a toggle, an append) calls `touch`
before it changes anything. The reason is the record: it is dumped from the entry, and
an entry holding only rows would write an empty block over a block the operator can
see. That is the one way this cache could destroy what it is supposed to keep.

## Log::evict — what a frame draws is what memory keeps

After a pane has drawn, `Log::evict(window, start..end)` drops the bodies of the blocks
that are entirely outside those rows: the blocks scrolled past, and the ones below the
view. `Panes::begin_frame`/`end_frame` complete the rule for a window nobody drew at
all -- a hidden session holds no bodies, except the one block it is still writing.
Its *records* are another matter, and the distinction is the whole reason this module
exists: a hidden window is still recorded, block by block as any other, and only its
bodies in memory are what a frame pays for. `Client::handle_event` used to drop a
hidden session's events before the sink saw them, which turned a view setting into a
hole in the journal; see `docs/notes/client-lib.md`.

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
