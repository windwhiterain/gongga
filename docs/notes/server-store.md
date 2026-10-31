# server/store.rs

## store.rs

The SPEC asks for data that is "structured for agentic retrieval/mutation", so
the transcript is not an opaque JSON blob: each session, message, tool call and
event is an addressable row with typed columns. A future tool can therefore
`SELECT` a session's tool failures or rewrite one message without parsing and
re-serialising a whole document.

The connection is behind a mutex and used synchronously. Every statement here is
a sub-millisecond local write, so the simplification costs nothing in practice;
if that stops being true the fix is `spawn_blocking`, not a redesign.

## interrupted_call

Written as an observation rather than an error: the command may well have run to
completion, and the model should verify rather than assume.

A template, asked for at read time rather than a constant stored with the row.
The difference matters for an old session: the placeholder is composed while the
history is being assembled, so a wording fix reaches a conversation that started
months ago -- and nothing in the database changes, which is the part a stored
sentence could never do.

## wire_item

One row's answer to "what do I send": a standard Responses input item, computed as
the row is written and stored beside the payload the reader reads. A request writes
those bytes and parses none of them, which is what lets it stream.

`None` is not "no item yet" but "no request is built from this row": a notice, the
prompt in force, a reasoning entry whose encrypted blob was never recorded, a
provider tool item whose binary payload was stripped on the way in. Deciding that at
write time is why the read path has no drop rules of its own.

Providers reject a `function_call` with no `function_call_output`, and a transcript
that reaches a request that way fails the same way on every later turn -- the same
`call_id` returning minutes apart with only a new `request_id` to tell the attempts
apart. Nothing here closes such a gap: the request builder says what happened at the
moment it writes (`docs/notes/server-body.md`), so the transcript keeps what did.

## migrate

`CREATE TABLE IF NOT EXISTS` cannot add a column to a table that already exists,
and a running harness must not lose its transcript just because the schema grew.

## Store::open

`:memory:` is taken literally: it has no directory to create, and creating one
called `:` would be the wrong answer to a caller that asked SQLite for a database
that never touches disk.

## Store::lock

A poisoned mutex means a prior write panicked mid-transaction; the WAL keeps the
database itself consistent, so recovering the guard is safe.

## Store::orphaned_blobs

A generated picture is a file, and files outlive rows unless somebody asks.
`heir` copies a transcript into a mentor session, so one file can be named by two
sessions; the answer has to be a query over every row, and it has to happen before
the delete. `DISTINCT` is not decoration: a session that re-sent the same item
would otherwise report it twice and `drop` would log a miss for the second.

SQL decides, `blobs.rs` unlinks. A store that deleted files would make a filesystem
layout part of what a database transaction means.

## Store::delete_session

`events` carries no foreign key, so its rows would outlive the session. That is
worse than a leak: SQLite hands a freed id out again, and the dead session's
events then pose as the new session's history.

## Store::get_session

A `WHERE name = ?` rather than "list every session and look for it":
`nudge_inboxes` asks this for each pending session on a two-second timer, and the
list query also runs a `COUNT(*)` per row, so the cost of the wrong shape grows
with the whole home instead of with the answer.

## Store::set_session_tuning

Both columns at once: they are one decision ("how should this session behave"),
and `set_session` is the only thing that writes them, so a half-applied tuning is
not a state anything else has to handle.

`model` is kept in step rather than derived on read: it is what list views show,
and `llm` being cleared has to put it back to the server's own model, which only
the caller knows.

## Store::history

This doc comment used to carry three overlapping openings (`Loads the transcript
in the order the model should see it`, the note replacement rule, and the
fact that no writer exists). The surviving statements:

A handover note, once written, stands in for everything that is no longer here, so
that is all a request needs. The server never deletes a row: only the `heir` skill
does, and only after it has copied every row into a mentor and compared the copy
row for row.

Production code always has a lower bound (a note), and a method nobody calls is a
second way to ask the same question.

## tool_result images

A tool result carries the text it always carried, plus `images`: a path and a
media type per picture, never the bytes.

That is the SPEC's "structured for agentic retrieval/mutation" taken seriously.
Base64 in a `messages` row would turn the transcript into a binary dump: a
picture is re-sent on every request of every later turn, and `get_messages` --
which the CLI prints -- would pour it into a terminal. A path is small, readable,
and lets the same picture be looked at again only when a request actually needs
it.

The cost, stated plainly: **the file is the state**. If the working directory is
cleaned, the branch is switched, or the temp file is deleted, the replay is
missing that picture. `crates/server/src/llm.rs` degrades it to a sentence naming
the path instead of failing the request, and a fold writes "an image was
shown here: <path>" into the note, so the model is never reasoning on as if it
had seen something that is now gone.

## MessageRow::parse — builtin_call

A `builtin_call` row is the one kind with no text of its own: nothing was
returned to it, because it ran at the provider. Leaving `text` empty made
`get_messages` print a line with no verb on it, and -- worse -- the row fell
through to the `unknown message kind` warning, so every read of the transcript
logged a complaint about a kind this server writes itself.

So the row gets a one-line description instead: `web_search "rust async book"`,
`web_fetch https://...`, `image_generation "a red cube on a table"`. It comes
from `activity_brief`, which is also what the live line uses (via
`activity_name` / `activity_detail`), so the transcript and the stream never
describe the same event differently.

## activity_brief

Kind name plus whatever the item says about itself, and nothing invented: a
missing query, missing URL and missing prompt are all left out rather than
rendered as `{}` or a bare space. An item with no `type` at all says
`a provider tool ran (its item was not recorded)`, which is true and tells the
reader the row is damaged; a blank line would read as "nothing happened here".

The status suffix (`(failed)`, ...) is kept, because a search that failed is not
the same row as one that found something.

## builtin_call

A provider tool item (`web_search_call`, ...), stored as the JSON the provider
sent plus whether it is still replayable.

Verbatim is the requirement: `input[]` has to contain the item the way the
provider produced it, because the provider already executed it and has no local
output to pair it with. A copy assembled from parts is a shape nothing validates
until the next request fails.

`replayable` is false when a binary payload (`image_generation_call.result`) was
stripped on the way in, which is why that field is not derivable from the item
itself.

## Transcript

A read-only connection walking one session's sendable rows, one keyset seek per
row, so a request never holds the transcript as a value. It brings its own
connection because it reads *while* the request is being written: taking the
store's would park the turn that is waiting on that write.

The window is closed at a seq when the walk is made, which is what lets a retry
replay the same bytes as the attempt it follows. Without it, a row landing
mid-request would make attempt two send a longer transcript than attempt one --
and a retry that is not the same request is not a retry.

A notice is written for the operator, not for the model: it explains why a turn
stopped. Feeding it back would put our own bookkeeping in the transcript the
provider sees, and make the model answer questions nobody asked. That is decided
where the row is written (`wire_item`), not here, so the walk has no drop rules
of its own.

## Store::last_seq

Where the transcript ends. A negative bound in `--range` is resolved against it at
read time, which is what makes `-1` mean "the newest row" rather than "one row
before some fixed number".

## Store::messages_between

Both ends stated. The cursor form this replaced (`after_seq` plus a count from
the tail) could also truncate, which was the one thing it did that a range
cannot -- and truncation is a policy, so it moved to the caller.

`last_seq` comes back with the rows, because a caller still needs to know where
the transcript ends: an empty window and an empty transcript are different
answers.

## the_upper_end_stops_the_window

A gap in `seq` cannot happen today -- `append_message` is the only writer and rows
go only with their session -- so this pins the query's contract rather than a bug
that was seen.

## Store::message_index

A fold needs the seqs to decide where the note may take over, not just the
contents.

## Store::set_context_tokens

Called with the number from `usage`, never with a computed one: the provider is
the only party that tokenises with the tokeniser it will actually bill against.

## Prompt, stamp, prompt, append_prompt

A request is built from three things, and only two of them were durable: the rows,
and the model's own answers. The prose -- the instructions and the handover note --
was rendered fresh before every request and kept nowhere, so the database could not
say what the model had actually been told. `Prompt` is that missing piece: the
rendered instructions with the project's rules already joined, the rendered handover
note, the directory the rules were read from, and a stamp of the note it was rendered
from. It is written as a `system` row, and `Store::prompt` returns the newest one.

The stamp is why the check is cheap. Comparing prose means rendering it, which means
reading the config and two rule files; comparing a hash of the note says whether a
fold happened without touching any prose. So a step that stores nothing re-renders
nothing -- which is the rule this module exists to enforce: what is in the database is
what was sent, and nothing re-decides it later.

`system` is excluded from history the way `notice` is -- it *is* the prompt, and
appending it as a row inside the transcript would send the instructions twice -- but
`messages_between` shows it, described: how many characters, with or without a note,
read from which directory. An operator reading a session back should see that the
prompt changed and where, without a 10k-character row in the middle of the dialogue.

## Store::add_pending_task

An aborted process is indistinguishable from a deliberate `cancel`: either way,
whatever is still in this table is what the next start owes.

## Store::take_pending_tasks

The caller is about to write the returned rows into the transcript: a message
must be either still in the inbox or already in the transcript, never both
(delivered twice) and never neither (lost).

`except` is the row a turn was started from. That message is already in the
transcript -- it was written when the turn began -- so handing it back here would
append the operator's line a second time.

## Store::add_detached_job

Written before the job is handed to the model, so a crash in between leaves a row
the next start adopts rather than a process nobody knows about.

## SessionRow::llm

`None` means "whatever the server is configured with", which is what most
sessions are.

## SessionRow::context_limit

`None` means the server-wide budget. The original line said "character budget";
the config field is `compress_above_tokens`, so the wording may be stale.

## MessageRow

Deliberately not the request's view of the same row: a caller that only wants
"what did it say" should not have to know about encrypted reasoning blobs or
reassemble the Responses API shape. It reads `display`, the payload as written,
while a request reads `content`. `text` carries whichever field is the
human-meaningful one for the kind (message body, tool arguments, tool output,
reasoning summary), so one column is enough to display any row.

## the_database_path_brings_its_directory

A path handed to `Store::open` has to bring its directory with it, and `:memory:`
has to be taken literally: creating a directory called `:` would be the wrong
answer to a caller that asked SQLite for a database that never touches disk. The
server itself can no longer be pointed at a database outside its home, so this is
the library's own contract rather than a command line's.

## finishing_one_pending_task_leaves_the_others

Two messages queued on one session used to share a fate: the first turn cleared
the session's pending rows wholesale and took the second message's row with it,
so a crash before the second turn ran lost the message from both places at once.

## a_folded_prefix_shrinks_the_request_without_touching_the_transcript

The transcript is a record, not a cache, and deleting the rows would lose the
only copy of what the session actually did.

The test folds through `fold_from_outside`: a second connection issuing the same
statement the skill issues. There is no setter to reach for, not even behind
`#[cfg(test)]`, because a method the store owns would be exactly the capability
this design takes away from the server.

## session_tuning_round_trips

`set_session --llm` and `--context_limit` have to survive a restart like
everything else. `model` follows the entry, because that is the column list views
read.

The two halves of a fold have to add up to the transcript. Splitting by
sequence does that by construction; counting rows did not, because a `notice` is
skipped and an unanswered tool call gains a placeholder.

Four rows, four items -- but not four rows' worth: the notice contributes nothing
and the unanswered call contributes two. That is exactly why a row count is the
wrong way to find the cut.

Clearing the entry goes back to the server's own, model included: the caller
passes what that is, because only it knows.

## opening_an_old_database_adds_the_new_columns

A database written before compression existed has to grow the columns rather than
fail to open, or every existing session would be stranded.

## the_measured_context_size_survives_a_restart

The context size is the provider's own number, so it has to survive a restart:
the first request of the next run is compared against it before any new
measurement exists.

## deleting_a_session_takes_its_events_with_it

Otherwise a deleted session's rows stay behind and, once SQLite reuses the id,
show up as the next session's history.

## a_call_interrupted_before_its_result_is_closed_for_the_model

A turn interrupted between "call this tool" and "here is what it said" leaves a
`function_call` with no `function_call_output`. Providers reject that outright
("No tool output found for tool call ..."), so the session fails the same way on
every later turn. The transcript closes the call on the way out instead.

## a_notice_is_readable_but_never_sent_back_to_the_model

A notice explains a stopped turn to the operator. It belongs in the transcript
they read and must not leak into the request the model sees, or the model ends up
replying to the harness instead of the user.

## tests::a_tool_result_keeps_its_pictures_by_path

A picture survives the round trip as a path and a media type -- the row holds no
bytes, and the test would see them if it did.

## tests::a_tool_result_without_pictures_reads_back_empty

Rows written before pictures existed still read back as plain results.

## tests::a_provider_tool_item_survives_a_round_trip

The provider's item comes back byte-for-byte: anything less is a request rejected
for a transcript that mentions a call nobody made.

## tests::a_provider_tool_row_reads_as_the_search_it_ran

The line a reader sees for a web search and for a page fetch, from a row written
the way the agent writes it.

## tests::a_provider_tool_row_never_renders_blank

Four damaged or minimal items, none of which may produce a blank row or leak a
JSON shape into the transcript: the display side must never be the place where a
reader learns nothing happened.

## the_report_reads_the_schema_so_a_tool_does_not

`Store::report` is the only place that joins `sessions`, `events`,
`messages` and `tool_calls` for a reader. A number it cannot find is reported
as zero: nothing is estimated, and a renamed column is a change here rather
than in every tool that reads a home.

