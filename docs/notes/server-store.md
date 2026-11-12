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

## The transcript is append-only, and the database says so

SPEC's rule -- `context is append only, context that has been sent must not be
change` -- is enforced by the schema, not by the callers. Two triggers on
`messages`, created after the migrations run:

- `context_no_update` -- `BEFORE UPDATE` raises, so a row that was written cannot be
  rewritten. There is no API that edits a past row.
- `context_cut_tail_only` -- `BEFORE DELETE` raises unless the row is the current
  tail of its session. A cut removes a suffix, one row at a time from the end; a
  delete that reaches into the middle is refused. Deleting a whole session is the
  exception: the cascade runs after the `sessions` row is gone, and that is removal
  of the session, not an edit of its context.

So the transcript has exactly two writes: append a row, and cut the tail
(`Store::truncate_after`). The migrations are the only `UPDATE`s, and the triggers are
created at the end of the same pass, so the open that creates them is unharmed -- but a
later open runs the migrations again with the triggers already there. Each one has to be
a no-op on a home that has been through it, and one that is not is a home that cannot be
started twice: `backfill_wire_items` read `content IS NULL` as "written before the
split", which is also how a `system` or `tools` row is spelled, so a second start tried
to rewrite the head it had already written and died on `context_no_update`. A convention
can be forgotten by the next caller; a trigger cannot.

## Store::write_head_once

The tool head is context too, and it is written once. `write_head_once` appends the
`tools` row only if the session has none, and refuses otherwise -- so "append another
schema" is not an operation the store has. `Agent::tool_set_in_force` reads the
stored head and returns it; only a session with no head stamps one. See
`docs/notes/server-tools.md`.

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
that reaches a request that way fails the same way on every later turn. Two places close
that gap, and both append: `agent::answer_interrupted` when a turn is cancelled while a
call is in flight, and `answer_open_calls` at every start, for a run that died before it
could answer. The second is SPEC's "auto recover invalid transcript (only append, no
drop)", and it is why the request builder no longer has to invent anything.

## drafts: what a stream has said so far

`drafts` is not part of the transcript: a row there is rewritten as chunks arrive and
deleted when the finished item is appended, which is exactly the opposite of the rule the
transcript lives under. It exists because the alternative is worse. The text of a
streaming answer lived in one process's memory, so a `kill -9`, a power cut or a watchdog
that gave up lost an answer the window had already drawn -- the reader saw a message that
the record says was never said.

`agent` writes an item's draft at most once per `DRAFT_FLUSH` (250ms), so what a kill can
lose is a quarter second of text rather than all of it, and a fast stream is not one disk
write per chunk. `settle_drafts` runs at every start, before anything serves: each draft
with text becomes an appended row and the table is cleared. Once, because the row is the
record now -- and the test that says so compares *ages*, since the pending turn is
replayed and streams an answer of its own.

An answer becomes a `text` row marked `interrupted`. A **thought** does not become a
`reasoning` row: a reasoning item with text and no `encrypted_content` is a shape the
provider rejects (`reasoning_text must be passed back`, measured in
`.agents/notes/2026-10-30-tool-call-replay-is-reordered.md`), so half a thought is kept as
a `notice` -- reader-only, never sent. The reader keeps what it saw; the request keeps
what the provider will accept. Measured over this machine's transcript: 1,320 reasoning
rows, all with their blob.

`open_calls` answers "what would a provider find unpaired" from the rows themselves, by
`call_id`, because that is what a provider pairs on. Measured over this machine's
transcripts: of 2,870 calls, two had no following row at all -- both from a run killed
inside a tool -- and **none** was followed by something other than its own output. That
is what makes positional reordering unnecessary rather than merely dropped: a tool runs
inline as its call arrives, so the rows are already in the order a provider takes.

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
`handoff` copies a transcript into a mentor session, so one file can be named by two
sessions; the answer has to be a query over every row, and it has to happen before
the delete. `DISTINCT` is not decoration: a session that re-sent the same item
would otherwise report it twice and `drop` would log a miss for the second.

SQL decides, `blobs.rs` unlinks. A store that deleted files would make a filesystem
layout part of what a database transaction means.

## Store::delete_session

`events` carries no foreign key, so its rows would outlive the session. That is
worse than a leak: SQLite hands a freed id out again, and the dead session's
events then pose as the new session's history.

The same rule for every row keyed by the session's *name* rather than its id --
`pending_tasks`, `detached_jobs`, `tool_jobs` -- and for `drafts`, whose id-keyed
row would otherwise be settled at the next start into a message for a session
that no longer exists. One transaction, because a delete that half-happened is a
session whose name is free and whose rows are not.

The rows of a detached job are the last handle on its process, so deleting a
session drops that handle before the process is confirmed gone: the stop carries
the job in hand instead, and a job whose stop fails is only a warning now. There
is no session left to retry for. See `docs/notes/server-tools-detached.md`.

## Store::list_sessions

The order is the turn that ended last, newest first: `COALESCE(ended_at, created_at)`
descending, `name` breaking the tie. A session that has not finished a turn yet stands
at `created_at`, so making one puts it at the top, and a window lays its panes out in
this order.

`ended_at` is written by `finish_turn` and by nothing else. That is the point of a
column of its own: ordering by `updated_at` looked like "recent activity first" and
behaved like "whatever was written last", because `set_session_tuning` and
`set_session_cache` rewrite it too -- choosing a model moved a session's row, and the
pane the operator was reading moved with it. The list is ordered by turns, so only a
turn may move it.

The tie is `name`, not `id`, because `name` is what a client is told: `SessionInfo`
carries `ended_at` and `name`, and `client::Client` sorts a delta by the same two
numbers, so a row that arrives on the wire lands exactly where the store would have
put it. `id` is unique but unpublished, and two orders that disagree are two orders.

The previous key was `created_at`, chosen because nothing rewrote it: a window's panes
are laid out in this order, so a name that moves takes the operator's pane with it.
That cost is accepted now: the session being worked on is the one to find, so the row
that finished a turn moves to the end of the list, where the next one will be.

A session with a transcript and no stamp is repaired at start, in `migrate`
(`backfill_ended_at`): its `ended_at` becomes the time of its last row, which is
written by its last turn. That condition is the whole of it -- a session with no
rows never ended a turn and keeps `NULL`, and the order says `created_at` for it --
so the repair is safe to run every start, and it is what makes an older build's
home right again after this one has stamped only the turns that ended since. The
first shape of this checked "no row is stamped yet", which was wrong on the home
that mattered: it had a handful of stamped rows and twenty-six old ones, so the
repair skipped itself and the list still read as "ordered by when sessions were
made". `updated_at` was the other candidate and is worse: it also moves when a
model is chosen, and it is written after the last row.

## Store::get_session

A `WHERE name = ?` rather than "list every session and look for it":
`nudge_inboxes` asks this for each pending session on a two-second timer, and the
list query also runs a `COUNT(*)` per row, so the cost of the wrong shape grows
with the whole home instead of with the answer.

## Store::set_session_tuning

## Store::set_session_mode

Its own statement, unlike the tuning, because the mode is not one of the model's
settings: it says what the session may do to the workspace, it is read per call rather
than per turn, and a session whose model is re-resolved must not have its mode rewritten
as a side effect.

A `NULL` column is `work` -- the mode of every session the column was added behind --
so an upgrade takes no rewrite and the read is one fallback.

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
a row: only the `handoff` skill does, and only after it has copied every row into a
mentor and compared the copy row for row.

Production code always has a lower bound, and a method nobody calls is a second way
to ask the same question.

## turn and step, as columns

A row states which turn wrote it and which step inside that turn. Both were only
inside `display`; as columns they answer "what did the last step leave open" without
walking the transcript, and `messages_by_step` makes that a range read.
`append_message` fills them from the payload, the way it already reads `call_id`, so
a caller states the pair where it knows it and stays silent where it does not: the
prompt in force, a notice, and the user's own line -- which opens a turn before the
turn has a number -- carry neither.

`backfill_steps` gives the rows written before these were columns the pair their
payload already names, once, on the open that adds them. A payload naming neither
stays NULL, which is how "stands in no step" is spelled, and a payload that is not
JSON is skipped rather than failing a whole home's migration.

`seal_calls` is the one way a row written before the columns gets an answer. It runs
on the open that adds them, *before* the ALTER: from that moment the question is
asked of a row's own pair, so a call written before them has to be answered while the
transcript can still be asked about it. Its own insert is deliberate -- `append_message`
writes the columns it is making room for. A crash in the middle leaves the columns
absent, so the next start asks again; asking twice answers nothing, because what it
writes is a result the next question sees.

`open_calls` then reads one thing: the newest `(turn, step)` in the session, and every
call in it that no result names. A turn answers its calls before it ends, so that is
the only place a half-written step can be. A resumed turn reuses its numbers, so an
older row sharing the pair is read too -- its answered calls filter themselves out by
`call_id`, never by position. `answer_open_calls` copies the call's own pair onto the
answer it appends, so the repaired row lands in the step it belongs to.

## pictures in a row

A tool result, and a message written with `@image(...)`, carry the text they always
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

## begin_turn and deliver_pending — the rows they answer

Both write the operator's own message as a `text` row with `role: user`, and both
answer its text beside where the transcript now ends (`OperatorRows`). The caller
publishes the text and only then announces the position: a window reads the
announcement as "I have drawn that row", so a row written and announced without
being sent is a line no window ever draws.

A row written behind the operator's message -- the live-compaction plan notice --
travels in the same answer, because it is the same kind of row: `role: user`, drawn
by `row_event` as an input line. The seq is the last one written, which is what a
watermark needs: "every row up to here" is not a claim about any one of them.


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

## Store::clone_session

One transaction, two statements: `INSERT INTO sessions ... SELECT` copies the source's
own facts -- model, placement, tuning, last measurement -- and `INSERT INTO messages ...
SELECT` copies its rows at their own `seq`. Nothing is read into Rust and written back,
so a copy taken while a turn appends holds every row or none, and the source cannot lose
a row to it.

`status` is the one column not copied: it says what the session is doing now, and a copy
is doing nothing. `created_at` and `updated_at` are the copy's own. `monitor_seq` and
`monitor_attempts` travel, because they describe rows the copy also holds -- a monitor
turn should not be handed the same rows twice.

What is not the transcript does not move: `pending_tasks` (work owed to the source),
`drafts` (a stream in flight), `subscriptions` (what the source asked to be told),
`tool_calls` (bookkeeping of runs) and `prefix_layout` (what the source's own name
learnt about its prefix). Pictures need no copy: a row names its copy by the `pictures`
id, and both sessions live in one home, so the clone reads the same bytes -- which is
also why `orphaned_blobs` may keep a file either session names.

An empty source clones to a session with no rows, which is what `set_session` makes
too. `rows` and `last_seq` come back so the command can say how much moved.

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

The row also carries `noticed`: whether the session has been told the job ended. It is
in the database because the second reader is the next start, and one end is one
notice even across a restart.

## Store::add_detached_call

The trace of a call that said `detached` and is not a process: there is no pid to
adopt, so the row is the only thing that can tell the next start the answer was lost.
Deleted in the same act that posts the report, which is what makes a surviving row
mean "nobody ever answered this".

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

## Comments moved out of the code

The comment checker holds a preamble to 120 characters and an `# Invariant` to 400,
so the argument behind a short claim lives here. Nothing below is new policy; it is
the text those comments used to carry.

**`keep_streamed` (`settle_handover`'s neighbour).** Both an answer and a thought are
kept as the rows a request is built from: a thought the model wrote is context it
should read back. A half thought carries no `encrypted_content` — it never arrived —
and that is accepted: measured, a reasoning item whose text is all it has answers 200.
Both kinds are appended last, so nothing lands between a `function_call` and its own
output; that is the one position rule with a 400 behind it, which is why
`answer_open_calls` runs first.

**A notice consumed by a turn.** A notice is advice about a measurement, not an
operator's message. Emptying the transcript advances `context_generation` even if the
old usage arrives late, so a notice whose measurement is stale is dropped rather than
spoken.

**`wire_item`.** A row's text is stored as written: the pictures it named stay in
`display`, and the provider's file for each is resolved when a request is built. A
file id in a row would stop working the day the provider deletes the file.

**`settle_handover`.** The note used to be prose this server printed ahead of the
transcript; a lesson is an ordinary message now, so no column holds request content and
the agent's own `send_message` is the only way one gets there. Appended rather than
placed at the front: the rows already in the transcript were sent, and a row is never
put ahead of them. One transaction, because a start that stopped between the insert and
the drop would append the same lesson again on the next one.

**`events_after`.** `after` is exclusive and is the log's `id`, not the session's
`seq`: one cursor has to mean one place in one stream, and the id is monotonic over the
whole table while `seq` is only monotonic per session. A reader that keeps the last id
it saw resumes exactly where it stopped, with no overlap and no gap. `kind` and
`session` are filters on the way out, never on the way in.
