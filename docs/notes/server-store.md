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

## INTERRUPTED_CALL

Written as an observation rather than an error: the command may well have run to
completion, and the model should verify rather than assume.

## migrate

`CREATE TABLE IF NOT EXISTS` cannot add a column to a table that already exists,
and a running harness must not lose its transcript just because the schema grew.

## Store::open

`:memory:` is taken literally: it has no directory to create, and creating one
called `:` would be the wrong answer to `--db :memory:`, which is how a throwaway
run keeps its hands off the operator's data.

## Store::lock

A poisoned mutex means a prior write panicked mid-transaction; the WAL keeps the
database itself consistent, so recovering the guard is safe.

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
in the order the model should see it`, the summary replacement rule, and the
test-only note). The surviving statements:

A summary, once written, stands in for everything up to its `summarised_upto`, so
that is all a request needs. The rows themselves are never deleted -- the full
transcript stays available to `history` and to anything that wants to audit it.

Production code always has a lower bound (a summary) or both (compaction), and a
method nobody calls is a second way to ask the same question.

## HistoryItem::ToolResult — images

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
the path instead of failing the request, and compaction writes "an image was
shown here: <path>" into the summary, so the model is never reasoning on as if it
had seen something that is now gone.

## MessageRow::parse — builtin_call

A `builtin_call` row is the one kind with no text of its own: nothing was
returned to it, because it ran at the provider. Leaving `text` empty made
`get_messages` print a line with no verb on it, and -- worse -- the row fell
through to the `unknown message kind` warning, so every read of the transcript
logged a complaint about a kind this server writes itself.

So the row gets a summary instead: `web_search "rust async book"`,
`web_fetch https://...`, `image_generation "a red cube on a table"`. It comes
from `activity_summary`, which is also what the live line uses (via
`activity_name` / `activity_detail`), so the transcript and the stream never
describe the same event differently.

## activity_summary

Kind name plus whatever the item says about itself, and nothing invented: a
missing query, missing URL and missing prompt are all left out rather than
rendered as `{}` or a bare space. An item with no `type` at all says
`a provider tool ran (its item was not recorded)`, which is true and tells the
reader the row is damaged; a blank line would read as "nothing happened here".

The status suffix (`(failed)`, ...) is kept, because a search that failed is not
the same row as one that found something.

## HistoryItem::BuiltinCall

A provider tool item (`web_search_call`, ...), stored as the JSON the provider
sent plus whether it is still replayable.

Verbatim is the requirement: `input[]` has to contain the item the way the
provider produced it, because the provider already executed it and has no local
output to pair it with. A copy assembled from parts is a shape nothing validates
until the next request fails.

`replayable` is false when a binary payload (`image_generation_call.result`) was
stripped on the way in, which is why that field is not derivable from the item
itself.

## Store::history_between

Compaction needs both bounds, and it needs them as one query: the mapping inside
is not one-to-one with rows -- a `notice` row is skipped, an unanswered tool call
gains a placeholder -- so "how many items does the old half hold" cannot be
answered by counting rows, which is how the summary used to cover a range that
was not the one it was asked for.

A call whose result never got recorded -- the turn was interrupted between asking
for the tool and hearing back -- cannot be sent as a bare `function_call`:
providers reject the whole request ("No tool output found for tool call ..."),
which makes the session fail the same way on every later turn. The placeholder is
the honest fix: it says what happened instead of pretending the call is open.

A notice is written for the operator, not for the model: it explains why a turn
stopped. Feeding it back as history would put our own bookkeeping in the
transcript the provider sees, and make the model answer questions nobody asked.

## Store::messages_after

`history_since` answers "what should the model see"; this answers "what happened
while I was waiting", which is what an IM bridge asks after a turn goes idle: it
knows the seq the turn started at and wants the assistant text that followed.
`last_seq` comes back with the rows so a caller can advance its cursor without a
second round trip (and without guessing: a turn whose only output was tool calls
still moves it).

Newest `limit` first, then flipped, so a long transcript yields its tail rather
than its head -- a caller that asked for 50 rows wants the 50 that just happened.

## Store::messages_between

`messages_after` states a lower bound and a count taken from the tail; this states
both ends. The difference only shows when a range is asked for that has rows
beyond it -- which `messages_after` would happily walk into, because a count has
no upper bound to stop at.

`last_seq` comes back from both, because a caller still needs to know where the
transcript ends: an empty window and an empty transcript are different answers.

## the_upper_end_stops_the_window

A gap in `seq` cannot happen today -- `append_message` is the only writer and rows
go only with their session -- so this pins the query's contract rather than a bug
that was seen.

## Store::message_index

Compression needs the seqs to decide where a summary may take over, not just the
contents.

## Store::set_context_tokens

Called with the number from `usage`, never with a computed one: the provider is
the only party that tokenises with the tokeniser it will actually bill against.

## Store::context

Returns `(None, full history)` when nothing has been compressed.

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

## SessionRow::compact_threshold

`None` means the server-wide budget. The original line said "character budget";
the config field is `compress_above_tokens`, so the wording may be stale.

## MessageRow

Deliberately not [`HistoryItem`]: a caller that only wants "what did it say"
should not have to know about encrypted reasoning blobs or reassemble the
Responses API shape. `text` carries whichever field is the human-meaningful one
for the kind (message body, tool arguments, tool output, reasoning summary), so
one column is enough to display any row.

## the_database_path_brings_its_directory

`--db <path>` has to bring its directory with it, and `--db :memory:` has to be
taken literally: that is how a throwaway run keeps its hands off the operator's
real database, and creating a directory called `:` would be the wrong answer to
it.

## finishing_one_pending_task_leaves_the_others

Two messages queued on one session used to share a fate: the first turn cleared
the session's pending rows wholesale and took the second message's row with it,
so a crash before the second turn ran lost the message from both places at once.

## a_summary_shrinks_the_request_without_touching_the_transcript

The transcript is a record, not a cache, and deleting the rows would lose the
only copy of what the session actually did.

## session_tuning_round_trips

`set_session --llm` and `--compact_threshold` have to survive a restart like
everything else. `model` follows the entry, because that is the column list views
read.

The two halves of a compaction have to add up to the transcript. Splitting by
sequence does that by construction; counting rows did not, because a `notice` is
skipped and an unanswered tool call gains a placeholder.

Four rows, four items -- but not four rows' worth: the notice contributes nothing
and the unanswered call contributes two. That is exactly why a row count is the
wrong way to find the cut.

Clearing the entry goes back to the server's own, model included: the caller
passes what that is, because only it knows.

## opening_an_old_database_adds_the_summary_columns

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

## messages_after_returns_a_window_and_the_tail

The read path an IM bridge uses: a window starting after a seq, ending at the
current tail, with that tail reported so the cursor can advance even when the
window turns out to be empty.

An empty window still reports the tail: "nothing new, and the transcript has not
moved" is a different answer from "start over".

The limit counts back from the end, so a long transcript yields the part that
just happened rather than a re-read of the beginning.

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
