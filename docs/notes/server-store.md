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

## Prompt -- written once, never compared

The head of a request is `instructions`, `index` and `tail`, and it is stamped with the
session's first prompt row and read from the store after that. There is no comparison,
because there is nothing to compare it against: a second rendering would be a second set
of bytes for a prefix the provider has already cached, and everything behind it would be
billed again.

This replaced `Prompt::rewritten_by`, which compared the three wire texts at every step
and appended a row whenever they differed. The rule was right about `cwd` -- a shell
reports one directory under several spellings and a spelling is not prose -- and wrong
about the rest, because "the text differs" and "a new row is cheap" cannot both hold
when the text is the front of everything the model has read

## interrupted_call

Written as an observation rather than an error: the command may well have run to
completion, and the model should verify rather than assume.

It is composed when a call is known to have no answer -- the turn was cancelled -- and
appended as that call's `function_call_output`, so the pair is completed in the
transcript rather than while a request is being written. A request that invents the
missing answer is a request that can invent a different one later.

## wire_item

One row's answer to "what do I send": a standard Responses input item, computed as
the row is written and stored beside the payload the reader reads. A request writes
those bytes and parses none of them, which is what lets it stream.

`None` is not "no item yet" but "no request is built from this row": a notice, the
prompt in force, a reasoning entry whose encrypted blob was never recorded, a
provider tool item whose payload could not be kept. Deciding that at write time is why
the read path has no drop rules of its own -- and a drop rule in the read path is a
silent rewrite of every item behind the one it drops.

A provider payload is spent here: a `builtin_call`'s payload is read back from its blob,
and one that cannot be read makes the item non-replayable now, at the insert, not at the
tenth replay. A picture is not: its text is stored exactly as the reader wrote it, and the
file the provider holds is resolved when a request is written, because that file expires
(`docs/notes/server-pictures.md`).

Providers reject a `function_call` with no `function_call_output`, and a transcript
that reaches a request that way fails the same way on every later turn. Nothing here
closes such a gap -- `agent::answer_interrupted` does, when the answer is known to be
missing -- so the transcript keeps what did.

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

## Store::list_sessions

The order is the order the sessions were made in, newest first, and it is the one
thing about a session no later work can change: a window lays its panes out in
this order, so a name that moves takes the pane the operator was reading with it.

Ordering by `updated_at` looked like "recent activity first" and behaved like
"whatever finished last", because `finish_turn` rewrites that column at the end of
a turn. Two sessions made in the same millisecond had no order at all between them,
and a restart that replayed owed work finished those turns in scheduling order, so
the same home came back drawn differently. `created_at` is written by
`create_session_on` and by nothing else, and `id` is unique, so the pair is a
total order that survives both the tie and the restart.

## Store::get_session

A `WHERE name = ?` rather than "list every session and look for it":
`nudge_inboxes` asks this for each pending session on a two-second timer, and the
list query also runs a `COUNT(*)` per row, so the cost of the wrong shape grows
with the whole home instead of with the answer.

## Store::set_session_tuning

Both columns at once: they are one decision ("how should this session behave"),
and `set_session` is the only thing that writes them, so a half-applied tuning is
not a state anything else has to handle.

The model is the column list views show, so it is written with the tuning rather
than derived on read, and clearing it has to put the session back on the server's
own model, which only the caller knows.

## Store::history

This doc comment used to carry three overlapping openings (`Loads the transcript
in the order the model should see it`, the note replacement rule, and the
fact that no writer exists). The surviving statements:

What a handover leaves is the rows that came after it -- nothing stands in for the
part that is gone, because it is not gone from the mentor. The server never deletes
a row: only the `heir` skill does, and only after it has copied every row into a
mentor and compared the copy row for row.

Production code always has a lower bound, and a method nobody calls is a second way
to ask the same question.

## pictures in a row

A tool result, and a message written with `@image:`, carry the text they always
carried plus `images`: a path, a media type and the id of the copy this home keeps
per picture, never the bytes themselves.

`user_row` is where a message's pictures become a row, and `images_of` is how a
row reads them back -- the same field both ways, so `deliver_pending` and
`begin_turn` store what `transcript` then sends.

That is the SPEC's "structured for agentic retrieval/mutation" taken seriously.
Base64 in a `messages` row would turn the transcript into a binary dump: a
picture is re-sent on every request of every later turn, and `get_messages` --
which the CLI prints -- would pour it into a terminal. A path is small, readable,
and lets the same picture be looked at again only when a request actually needs
it.

The file is no longer the state, and that changed on 2026-11-03: the bytes are
copied into `<home>/pictures` when the row is written, and `image::files`
reads that copy. A working directory that is cleaned or a branch that is switched
now costs nothing, and a file rewritten at the same path does not turn an old
message into a new picture. `docs/notes/server-pictures.md` has the reasoning;
the `pictures` table holds the copy's ids and the path it was deduped on.

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

## Store::truncate_after

The other half of the retry rule. `llm.stream` retries a failed attempt against
the window that was pinned before it was sent, so a row past that number belongs
to the failed attempt alone and is deleted before the next attempt runs --
otherwise the transcript replays half an answer the model never gave.

Only `messages` rows: the attempt's tool calls ran inside that window and their
`tool_calls` rows are the record of what actually happened on the machine.

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

## Store::set_usage_for_generation

Called with the token counts from `usage`, never with a computed one: the provider
is the only party that tokenises with the tokeniser it will actually bill against.
The rate and the delay are this server's own clock, measured around the same
request, and all four are written in the same statement: they are one measurement,
and a row that held the context of one request beside the rate of another could not
explain itself. `tps` and `ttft_ms` are the columns SPEC's `tps (latest)` and
`ttft (latest)` are read from, and NULL is "not measured" -- a provider that reports
no token counts leaves them NULL rather than 0.

The update also names the generation captured when the request started. If a tool
handed the transcript over while the response was still streaming, that response's
usage belongs to the old context and the conditional write does nothing. Provider
usage events still record what the request cost; the session's current context is
not overwritten with a transcript that no longer exists.

`cached_tokens` is stored as the count the provider reported beside the input, not
as the share SPEC's title shows: the share is that count divided by this one, so
keeping the pair keeps the division possible and reversible.

## Prompt, prompt, append_prompt

A request is built from three things, and only two of them were durable: the rows,
and the model's own answers. The prose was rendered fresh before every request and
kept nowhere, so the database could not say what the model had actually been told.
`Prompt` is that missing piece: the rendered instructions with the project's rules
already joined, the index behind them, the start card, and the directory the rules
were read from. It is written as a `system` row, and `Store::prompt` returns the
newest one.

`system` is excluded from history the way `notice` is -- it *is* the prompt, and
appending it as a row inside the transcript would send the instructions twice -- but
`messages_between` shows it, described: how many characters, read from which
directory. An operator reading a session back should see that the prompt changed and
where, without a 10k-character row in the middle of the dialogue.

## Store::add_pending_task

An aborted process is indistinguishable from a deliberate `cancel`: either way,
whatever is still in this table is what the next start owes.

## Store::deliver_pending

An operator's message and a background job's result remain owed until they are
appended. A context notice is different: it is advice about a measurement, and a
handover can make that measurement obsolete while the notice waits in the inbox.
Marked notices carry their context generation and token limit. A database trigger
advances the generation whenever a handover resets a nonzero context to zero.
Delivery compares both fields with the session's current row. Usage writes are
conditional on the request's generation too, so an old response cannot restore
its high token count after a handover. A stale notice is deleted without becoming
a user message; unmarked injections are always appended. Every row leaves the
inbox exactly once, and the return value counts messages actually appended.

## Store::take_pending_tasks

The caller is about to write the returned rows into the transcript: a message
must be either still in the inbox or already in the transcript, never both
(delivered twice) and never neither (lost).

`except` is the row a turn was started from. That message is already in the
transcript -- it was written when the turn began -- so handing it back here would
append the operator's line a second time.

## Store::pending_injects_of

A read, not a delivery: the `wait` tool asks what has arrived, and what a step
boundary does with it is `deliver_pending`'s call. Only `inject` rows -- a `turn`
row is the turn's own obligation and is pending for as long as the turn runs, so
counting it would report news on every call of a session that is merely working.

## Store::add_subscription and friends

A subscription is data like any other: the row survives the process, so a restart
resumes what sessions asked to be told about, and `del_session` drops a session's
rows because there is nobody left to tell. `created_at` is written and never read --
it is when the session asked, which is what an operator reading the table by hand
wants to know.

## Store::add_detached_job

Written before the job is handed to the model, so a crash in between leaves a row
the next start adopts rather than a process nobody knows about.

## SessionRow::model

The whole model, `provider/model_name`, because a name alone does not say who serves
it. A row written before the column said that keeps its bare name in `model_name`,
and the select reads `COALESCE(model, model_name)`: the same model, named as well as
that row knew how.

## SessionRow::context_limit

`None` means the server-wide budget. The original line said "character budget";
the config field is `compress_above_tokens`, so the wording may be stale.

## SessionRow::usage

One `protocol::Usage` rather than four loose fields, because the values are one
measurement: `context_tokens` is the provider's billed input, `cached_tokens` what
of it came from its prefix cache, `tps` is output tokens over the time the stream
took, and `ttft_ms` is the wait for its first token. A session that has never
completed a request has 0 context and no rate, and SPEC's title says nothing
rather than zero.

## MessageRow

Deliberately not the request's view of the same row: a caller that only wants
"what did it say" should not have to know about encrypted reasoning blobs or
reassemble the Responses API shape. It reads `display`, the payload as written,
while a request reads `content`. `text` carries whichever field is the
human-meaningful one for the kind (message body, tool arguments, tool output,
reasoning summary), so one column is enough to display any row.

## a_folded_prefix_shrinks_the_request_without_touching_the_transcript

The transcript is a record, not a cache, and deleting the rows would lose the
only copy of what the session actually did.

A fold is a second connection issuing the same statement the skill issues. There is
no setter to reach for, because a method the store owns would be exactly the
capability this design takes away from the server.

## a_call_interrupted_before_its_result_is_closed_for_the_model

A turn interrupted between "call this tool" and "here is what it said" leaves a
`function_call` with no `function_call_output`. Providers reject that outright
("No tool output found for tool call ..."), so the session fails the same way on
every later turn. The transcript closes the call on the way out instead.

## a_notice_is_readable_but_never_sent_back_to_the_model

A notice explains a stopped turn to the operator. It belongs in the transcript
they read and must not leak into the request the model sees, or the model ends up
replying to the harness instead of the user.

## the_report_reads_the_schema_so_a_tool_does_not

`Store::report` is the only place that joins `sessions`, `events`,
`messages` and `tool_calls` for a reader. A number it cannot find is reported
as zero: nothing is estimated, and a renamed column is a change here rather
than in every tool that reads a home.


## A failure, and what a monitor turn has already been told

Two columns on `sessions` carry SPEC's monitor: `monitor_seq` is the last row a
monitor turn was handed, and `monitor_attempts` is how many attempts that search
has made for the current failure.

They are columns rather than a table because a turn writes its user row before it
starts, so `seq` only grows: `monitor_seq < last_seq` is exactly "a failure nobody
has been told about", and it survives a restart that a broadcast subscriber would
not. `finish_turn` clears the counter and pushes the marker past the transcript
whenever a turn ends *not* in error, which is what makes a later failure a new
episode. See `server-monitor.md`.
