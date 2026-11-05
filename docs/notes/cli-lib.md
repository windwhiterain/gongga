# cli/lib.rs

## lib.rs

This crate owns the command surface: it takes the catalog the server publishes,
turns one command's tokens into a message on the wire, and keeps no state of its
own -- the sticky arguments, the drawn sessions and `cli.toml` are the window's
(`docs/notes/tui-state.md`). Which is which is the published `target`, not a
list here -- so a command a script registered is handled by the same code path
as a built-in, and none of them needs a client rebuild.

It does not own the *input line*. Whether a line is a `/command` or prose for
the agent is the interactive front-end's decision (`Client::command` and
`Client::message` are the two things that decision can ask for), so the
one-shot path -- `gongga-cli send_message main hi` -- never has to guess: its
argv is already tokens.

It knows nothing about terminals. That is the point of the split: anything that
can present `Sink` events can drive it, and the interactive client is one such
thing (the window is `gongga-tui` in `crates/tui`, and the command line is
`gongga-cli`, this crate's own `src/bin/gongga-cli.rs`). Nothing here names a rendering
crate, so the command layer builds into a script, a test, or a UI without
dragging a terminal along.

Wire encoding stays here too: `Link` is the only thing that speaks the
protocol, which is what lets a front-end hold a link without learning what a
frame is.

`parse_argv` names the program twice, as clap's `name` and as its `bin_name`,
because `bin_name` is the word clap prints in `Usage:` and it defaults to
`argv[0]`: without it a bundle's `gongga cli --help` answers as
`gongga-cli.exe`.

## Connection

The parts of the command line that say *where* to connect.

Separate from `Args` so the resolution rules -- flag, then environment, then
config.toml -- have one definition. Both binaries flatten it into `Args` today;
a front-end that wants the connection without the command surface could take
`Connection` alone.

Deliberately *not* a doc comment: clap folds a flattened struct's `///` text
into the parent command's about line, which would replace the program's own
description with this paragraph.

## Connection::endpoint

Resolved once per process: a running client does not re-read config.toml between
retries, because a file edited mid-outage would make two attempts of the same
reconnect mean two different servers.

## Args::check_flag_order

clap hands an external subcommand every token after it, so `gongga-cli send_message
--server 1.2.3.4:1` would take `--server` as one of the command's own arguments
and then quietly dial the default server.

## Sub::Cmd

Anything but a flag is the command itself, because what the client can run is
decided by the server and only known after the handshake: there is no fixed list
of subcommands to declare here.

`gongga-cli set_session --name main`. One command per process, the answer on stdout
as JSON lines, then exit -- this is how a script (or the model in a `bash` tool)
drives the harness.

## LineKind

This is the command layer's own vocabulary: the layer that reads events is the
one that knows a line came from a tool call or a failure, so it says so. A
front-end turns that into a colour; it does not decide what it is looking at.

`Command` is the line that was *asked for* -- `/cmd` as typed, or `$ cmd --args`
as it ran -- and it is deliberately not `System`. The status word and the
connection notice are the harness talking; the command is the operator talking,
and one colour for both leaves neither findable. See `docs/notes/tui-render.md`
for the whole palette and why no two kinds share one.

## Sink::note

The harness talking about itself, as opposed to a line in a window: connected,
refused, a build mismatch, a server log line. It is a separate method because the
destination is the front-end's decision -- a line that names no session still needs
a place, and "the command line" is the wrong place: SPEC gives that row to `cmd`
and `cmd_result` only, and it keeps one entry.

The one-shot mode still says them, on stderr, so a pipe of JSON keeps carrying
exactly the events.

## Sink

The interactive client (`crates/tui`) implements it with styled lines; one-shot
mode emits the events themselves as JSON and renders nothing. Neither
implementation knows what produced the text.

`stream` takes a `prefix` next to the text, and the front-end prints that prefix
**once per block**: `Log::stream` in `crates/tui` writes it when a key first
opens a block, then appends later chunks of the same key bare. So anything that
belongs at the start of a block -- and nothing that belongs in its body -- is a
prefix.

The label is kept *out* of the text and printed on the block's top visible row, so a
block that grew past its cap still says what it is. It used to be the block's first
row, which meant a capped block showed its newest rows and no label at all.

## Sink::stopped

A turn that ended is told to the front-end once, on the same path as every other
fact: `Event::Status` with a status that is not `running` is a stop, and the sink
is told the session's name and whether it failed.

It is a hook of its own because a stop is the one fact a window cannot draw: the
session's own line says "idle" in a window the operator may not be reading. A
front-end that wants to say so outside its window -- the TUI's toast -- needs the
fact, not the line that carries it; a front-end that does not care ignores it, and
the one-shot mode, which prints the events themselves, never hears it.

Told once per turn that ended, and never for a turn that is still running: a
session that is working has not stopped.

Nor for a turn somebody stopped. `Event::Status` carries `outcome`, and
`TurnOutcome::Cancelled` -- a `cancel`, an `interrupt_session`, the window that owned
the server leaving -- is not a stop worth announcing: the operator asked for it, so
telling them it happened is noise, and marking the session yellow says it finished
out of sight when it finished because they looked at it. The wire carries the word so
that this decision is not a parse of `detail`, which is a sentence for a person.

## Client::handle_response

Who is speaking belongs on that prefix, not in the text: a session tag in the
text would be repeated by every delta. The prefix is `[<session>] ` followed by
the part's own label (`thinking: `, `tool: `, or nothing for output), which is
the same shape `Event::Status` already uses for `[<session>] idle`. The stream
key keeps the session for routing (`r:<session>:<turn>:<part>`) and does not
depend on the tag being shown.

A row `get_messages` reads back arrives on this same path, under `REPLAY_TURN`.

Shown for a single session too. The tag costs one line start per stream, while
turning it on by session count would change how the lines of a session already
on screen look the moment another session is created -- and a prefix must stay
fixed for the life of its key.

## Client::handle_response — one part, one block, one window

A part of a response is its own block (`r:<session>:<turn>:<part>`), and the
*window* is what keeps sessions apart. The older rule folded a session's non-answer
parts onto one shared key so a front-end could draw them as one row; that grouping
was a workaround for having one window for everybody. With one window per session the
key has nothing left to do, and the answer no longer needs a key of its own.

Dropping it also drops the front-end's rewrite-on-new-label hack: a block is one
label's text, so a chunk wearing another label opens a new block where the old one
was sealed. Two events can never be glued onto one row.

## Client::tag, Client::session_window, Client::command_window

The label says which part of an answer this is. The name is the window's job: every
area the client can send a line to is labelled with the session it belongs to -- the
list column, a `full` pane's title, a `line` group's row -- so the text never carries
`[<session>] `. It used to, for the two modes that had no header; once the window
bracketed the name in all three places, keeping it in the text printed it twice
(`[peer] [peer] hi there`).

`command_window` is the window for a line that names no session: the server's log
lines, every refusal the client makes, and the commands addressed to no one.
Its name grows a space if a session already owns it, so a session named `cmd` cannot
take the command line's window.

## Client::command_line — where a command is read

A command that named a session is read in that session's view: its echo, the rows a
read returned, and its result line belong next to the transcript they came from. The
command line keeps the commands that named nobody, which is what it is for -- a ticker
of what the operator asked the *client*.

The result event carries only the command's id, so the window is decided when the echo
arrives and remembered until the answer does (`Client::results`). A link that dies
mid-command forgets the map: an answer that will never arrive has nowhere to go.

## Client::answer_window -- the window may be gone

`del_session` publishes the shortened session list *before* its own result, so by the
time the answer arrives the window it was routed to belongs to a session that no longer
exists. Writing there would be invisible -- no pane draws that name any more -- and it
would leave a row in the log under a name the server hands out again, which is exactly
what the next session under that name would open on.

So an answer keeps its window only while that window is one the client draws: a live
session's, or the command line's. Anything else is said on the command line, where the
commands that named nobody are read too.

## Client::set_sessions and Sink::forget

Both announcements -- the greeting a redial brings and the streamed `Sessions` -- carry
every session, so a name in the old list and not in the new one was deleted. That is
the only signal a client gets, and it is enough: `set_sessions` compares the two before
replacing one with the other, and tells the front-end to empty that name's window
(`Sink::forget`).

The signal has to be the deletion, not the layout. A layout stops drawing the sessions
whose window is hidden, and their lines stay -- that is the announcement's own
invariant, "what a frame shows, never what is held". A deleted session differs in the
one way that matters here: the server frees the name, and the name is what the window
is called.

The whole `SessionInfo` goes to the completer, not just the name: a value completed for
a session belongs to that session's directory and machine, and the completer has no
other place to learn them. See `docs/notes/cli-catalog.md`.

## Event::Models and Event::Commands — an edited roster

A roster edit arrives as two events on the connection the window already holds: the
table, and the catalog -- because an argument publishes the values the roster lets
through (`ArgSpec.accepts`). The client answers each by replacing what it holds. Nothing is said in the log: the
operator edited a file, the server noticed, and the next Tab differs from the last one.
The popup itself is not recomputed here -- it is rebuilt from the catalog on the next
keystroke, which is the same rule every other value follows.

## Client::set_sticky_vocabulary

Two facts the front-end supplies, and this layer stores neither: the sticky argument
names (what `set_sticky <type>` completes) and the session a bare message goes to
(what makes `@skill:` complete the skills of the session the message will reach). A
message names no session on its own line, so that entry is the only address it has.
The window calls this after any change; nothing here reads `cli.toml`.

Where the skills are read moved out of here and into the catalog, keyed by the
directory they came from. Pushing a list in from this function made its correctness
depend on every caller remembering -- the greeting ran it, a session that moved did
not.

## Sink::layout

Defaults to a no-op: a front-end that shows every row in one window has to know the
layout does not exist, and the one-shot sink must not start rendering. It is pushed on
every `refresh`, which is how a window that attached late still hears about it -- and
it carries the sessions the *front-end* asked to see, because which are drawn is the
window's state (`docs/notes/tui-state.md`).

The layout is announced as SPEC's lists rather than as a `split` flag, because the
front-end has to know the names and their order anyway -- and a flag would leave it
inventing a naming rule the command layer already owns. `shown` narrows the announced
windows with the lines it narrows: a pane that shows nothing is not a pane. The lines
carry their window either way: the announcement says what a frame shows, never what is
held.

Both lists stand oldest first. The store publishes sessions newest first, and the
frame reads them the other way round -- what arrived last is at the bottom of the list
and at the right of the split, as a transcript appends.

## A read's rows need no handler

`get_messages` answers with rows on the stream, one event each, before the result that
says how many -- and each is the event that row was published as when it arrived, so
`handle_response` and `handle_tool_answer` draw them and `Event::Log` prints them. The
command layer has no third path for a line, which is the whole argument for reusing the
vocabulary instead of adding a row event: a `message` event was one more shape every
front-end had to learn, and the shapes are where a replay and a live stream drift apart.

What the replay loses is the row's `seq`, which used to sit in the label. A reader who
wants to know where the transcript was read asks with a range and is told by the
result's `last_seq`; the position was never something the eye needed per line. What it
keeps is the turn: `REPLAY_TURN` says a part is a row rather than a stream, and that is
the difference between a block of its own and text appended to the answer a running
turn is still writing.

## ask

`run_once` prints every event it meets, because one-shot mode's answer *is* the events.
`skill ls` wants one command's message and none of the news around it, so `ask` is the same
send-and-wait with the printing removed: it feeds each event to the client (a sticky still
follows) and returns when the `cmd_result` carrying this command's id arrives. Anything else
on the link is not this question's answer, and a table never carries a session's news.

## kind_of

The one place a server-named `HeadKind` becomes one of this crate's `LineKind`s, and
therefore the place where "how this is drawn" begins. It is a style decision, so it is
here and not on the wire: the server says a call failed, and this crate decides that is
a tool error rather than an error of the harness.

## Client::handle_tool_call

One block per call, keyed `c:<call_id>` and labelled with the words the server sent
(`bash·check the state, name=default `). The key is the call and not the turn: SPEC's `abstract`
holds a call and its result together, and the id is the only thing they share -- two
calls in one turn are two items, not one block that keeps growing.

The block opens and closes in one call (`text: ""`, `done: true`): a call is a fact,
not a stream. Its content is the tool's output, which arrives as `tool_answer`.

## Client::handle_tool_answer

A call and its result are two separate, independently streaming blocks. The call
uses `c:<call_id>` and the result uses `t:<call_id>`; the shared `call_id` is their
pairing, not a reason to share a body or a fold. `Sink::tool_call` passes the
structured head so the call can update its own arguments and summary in place.
The result's title carries the paired call summary, so it remains identifiable
without being nested in the call's expanded frame.

Every stdout or stderr chunk appends to the one result block. Output streams do
not decide success or failure and never split a result by pipe. Only the final
`Head` supplies ✓/✕ and elapsed time, immediately after the result's status mark.
The final verdict relabels and seals that same result block; an empty result has
a summary and no fabricated output text.

## Event::Log — no tag

A server log message carries its own name already (`agent.rs` formats
`"{name}: delivered a message ..."`), so prefixing the session would render as
`[peer] peer: ...`. Nothing is added here. `Event::Cmd` and `Event::CmdResult`
are left alone for the same reason in the other direction: they echo what this
client typed, not something a session said.

## NullSink

What a script gets is the event stream itself, one JSON object per line, emitted
where the events arrive. Anything the client renders for humans (banners, status
lines, streamed text) belongs to the TUI only -- except errors, which are the
one thing a caller with no screen still needs, since a line rejected before it
ever reached the server produces no event at all.

## Submission::Offline

Distinct from a rejection: the line is valid, its turn had simply not come, so
the front-end puts it back in the prompt instead of treating it as a mistake.

## Client::unconfirmed -- a line the server never answered

A line leaves the input when it is sent, and until its `cmd_result` arrives the
words are still the operator's: `submit_tokens` records `(id, typed line)`, the
`cmd_result` arm drops it, and a link that ends first hands it back. Without that,
a message typed while the server was leaving looked sent -- echoed, no answer, gone
-- and nothing in the window said which line it was.

`set_offline` is where they come back: it reports them on the command row
(`no answer for /events -- the server never said it had it`) and returns them so the
front-end can put the newest into the prompt. A row, not a note: the next dial
writes the status, and a report that a retry erases is no report.

Not replayed on reconnect, on purpose. The server cannot tell a command it never
read from one it read and answered into a dead socket, so a replay would double a
`send_message` whose *answer* was lost. The same words cost one Enter.

The report says "no answer", never "not stored": the datagram may well have arrived
before the link died, and the operator is the one who decides to send it again.

## Client::connected

Everything else the client knows about the server -- the catalog, the sessions,
the model -- is what the last greeting said. While this is false that knowledge
is stale by definition, and the status line says so rather than showing a model
number as if the server were still there.

## Echo

Both halves of the grammar reach the same code; the only difference the operator
should see is that a command looks like a command (`/set_session main`) and a
message looks like what they wrote. The wire never sees this distinction: a
message is `send_message <session> <text>`, the session passed in by the front-end
that keeps it, named explicitly so the text never lands on the session argument.

## Client::apply

The API a window's gestures use. An operation is not a command line: the window says
what it means. Only one of the three is the server's -- delete a session -- and the
other two are the window's own state, answered by the window before this is called
(`docs/notes/tui-run.md`). Answering them here too would make this layer a second
owner of `cli.toml`, which is what the split removed.

## Client::handle_bytes

Decoding lives here so a front-end never has to name a `ServerMessage`: it
passes the bytes it got from `Link::recv` and reads the result off the screen.

## Client::completer

The catalog is the same object, shared rather than copied: when a greeting
replaces the server's half, the popup sees it without the front-end being told
anything.

## Client::handle — the greeting

A greeting *is* the recovery. There is no separate resync path: the same message
that starts a session also hands back the catalog and the session list, so a
client that redials after a restart comes up knowing exactly what a fresh one
does. That is the whole reason reconnecting is cheap.

## Client::handle_event

Nothing is filtered here. Every event goes to the front-end, whichever session it
names, and `set_render_session` decides what a *frame* draws -- never what the client
is told. State and lines take the same path: a `Status` updates the list and writes its
row, a hidden session's answer streams into its window like any other.

The first shape of this dropped the events of a hidden session before the sink saw
them, reading "hide the window" as "hide its lines". It is the window that is hidden,
and the cost was invisible until it was paid: a front-end that records what it is
handed -- the TUI's journal -- stopped recording that session's answers and tool
output, so a session hidden while it worked came back with a gap in the middle instead
of the rows it had said. Nothing errored, which is what made it look like the session
had stopped.

The gate was also lopsided, which is how it stayed invisible: it asked
`Event::session`, and a `cmd` or a `cmd_result` answers `None` there -- its window is
the session its *arguments* name. So a hidden session kept the operator's command
lines and every answer to them, and lost exactly the conversation in between.

One event is more than a line: a `Status` that is not `running` is a stop, and it
reaches `Sink::stopped` as well (see `Sink::stopped` above). What a hidden session did
while the operator was not looking is exactly what the operator cannot read from the
window, so the fact travels on its own.

`run_once` is on this path too, and needs nothing extra: the sink it hands over draws
nothing, and which events reach a pipe is `prints`, decided after this.

## Event::Usage

A delta on the session the numbers belong to, then `refresh`: the title is part of
the layout, so the layout is announced again. Nothing is filtered and nothing is
written to a window -- SPEC gives these three numbers to a title, and a line in
the log would be a second place saying the same thing.

It is a delta rather than a republished `Sessions` for a reason the window would
feel: every announcement re-lays the panes out, and one per model step would
interrupt a reader who scrolled up while the answer was still being written.

## Client::run_event_ui

`set_event_ui`'s three arguments are three settings, so the command is a loop over
them rather than three commands with one name. Two rules make that legible:

- **Everything parses before anything saves.** `--block_size 9 --split sideways` is
  refused whole, because half an applied command is an events area nobody configured
  on purpose -- and the refusal does not report the setting it left alone.
- **The answer is the whole setting**, on a write as on a read: `block_size = 3
  split = false` in one line, because two answers to one command is an argument.

## Client::session_named

Which argument of a command names a session, read off the kind: `set_session` binds
`--name` and `get_messages` binds `--session` to one idea, and only the kind says which
is the session. What is *done* with the answer is the window's
(`docs/notes/tui-run.md`): a session the server announces is one nobody asked for, so
it is not drawn; a session a command names is drawn and becomes the sticky one.

`session_named` reads the *kind* the catalog gives an argument, not the argument's
name: `set_session` binds `--name` and `get_messages` binds `--session` to one idea,
and only the kind says which is the session. It answers `None` for a client command,
because `set_render_session` has a session argument too and it says whether that
session is drawn -- following it would undo what it says.



## QuotaRow, quota_row

SPEC's `active_quota` row, spelled once for every front-end, for the same reason
`Listed::usage` is: a rate, a share and a currency are formatted in one place, and
two front-ends cannot disagree about how many decimals a balance has.

The spelling is `provider · month 75% · week 40% · 5h 90% · credit 37.50 · money
¥215.52`, with SPEC's own labels and only the figures the provider reported. A
balance with no cents keeps none (`money ¥215`) because a whole number of yuan
printed as `215.00` reads as false precision; a credit with fractions keeps two,
because the GOAT plan's monthly credit really is `37.50`.

`kind` is the row's colour, and it is the command layer's decision: a row with
figures is a fact (`System`), a row that only names the provider and a complaint is
a failure (`Error`). A note beside real figures -- "the weekly window is
exhausted" -- stays a fact, because the numbers are still true.

## Event::Quota

One line, updated at every step's end, and the layout is announced again only when
the spelled row changed: a provider that reports the same balance on two steps does
not make the window repaint.

## Listed::status

The status word SPEC gives a session -- `idle`, `running`, `error` -- spelled here as a
word rather than a protocol enum, so a front-end that imports no protocol type can
still draw it. A window turns it into one column of the list row (a mark, not a word);
which of the three means what is the window's decision, not this one's.

## Listed::title

SPEC's `full` pane title, spelled once: what the last request measured -- `ctx`,
`cached (in n%)`, `tps (latest)`, `ttft (latest)` -- then what the session runs on
-- `cwd`, `model`, `thinking_effort`, `context_limit` -- in SPEC's order, the
last one shortened to `ctx_limit` so the title stays one row. Spelled here rather
than in the window for the same reason `status` is a word: a front-end
that knows no protocol type can still draw it, and the two front-ends cannot
disagree about how a rate is rounded.

`title_note` spells it: `ctx 12345 · cached 89% · tps 41.6 · ttft 820ms · cwd
C:/work/svc · model deepseek-official/deepseek-flash · thinking high · ctx_limit 70%`.

`cached` is a share, not a count: SPEC says `in n%`, and the client is where the
division happens because the wire carries what the provider measured (two token
counts) rather than a number derived from them. `cwd` is the session's own directory
and says where a turn's commands run. `model` is the model the session would spend,
provider and all: whose balance a turn draws on is the provider half, and which
brain answers is the pair.
`thinking_effort` is the level the session was given, which is the level its request
carries: the server resolves a model's roster default into a stored level when a
session is created or its model changes, and does not add one at display time. A
figure the last request did not produce is left out rather than shown as zero --
an empty context and a session that has never made a request are different facts,
and `ctx 0` cannot say which it is.

## Client::layout

SPEC's window, from one place: `listed` is every session's name with its status and
its numbers, and `shown` is the ones that also get a header and their events beside
the list.

The two lists overlap on purpose. SPEC's session list is every session -- a click on a
name toggles its window -- so the list is not a list of what is hidden. The earlier
shape gave each render mode one area and kept a name out of the list when another area
drew it, which made the list the only way back to a hidden session and cost the window
two more areas (`line`, and the modes as words).

The flag is read off each session rather than off the file's entries, because a
session nobody set is hidden: reading the map would leave a new session with no window
at all.

## Sink::settled

Every `cmd_result`, by id and outcome. A notification, not an instruction: a front-end
that keeps something for an accepted command -- SPEC's sticky follows an accepted
`set_session` -- hears it here while the answer is still news. What it does with it is
its own, because what it keeps is.

## The write-back a window does

SPEC's `set_session` carries a second sentence: it also sets the sticky entries --
every argument with a kind that the accepted command *typed*, keyed by kind -- which
is what keeps `send_message` from needing the session named every time. The write runs
on the acknowledgement, never on the submission, because a refused `set_session` must
leave the window pointed where it was.

Only typed arguments count, and only a different value is a change: a value the sticky
itself supplied is already in the file, and writing it back as stated is what kept a
`thinking` alive across a model switch. `del_session` follows nothing; which session a
command goes to is the sticky's to say, and SPEC names only `set_session`.

## Client::submit_tokens

Nothing else is followed by the submission path: naming a session writes no window,
and `set_render_session` is the only command that changes what the events area draws.

One echo per command. A client command is drawn where it answers -- the command
line. A server command is drawn from the `cmd` event the server sends, which is
the same line with the arguments the sticky filled in; drawing the typed line as
well put every command on the screen twice.

## Client::refresh

Deliberately has no "current session" in it. The only thing that ever stood
there was the sticky `session` argument, and printing it turned a persistant
argument into something that looked like a mode the UI was in. What is left are
facts about the server: how many sessions exist -- and, when there
is no link, that there is no link, because a session count with no server behind it
is a lie the operator cannot see through.

## Client::refresh — no footer

There used to be a second line under the status, listing the sticky arguments, the
block size and every session whose render mode was not the default. It is gone with
SPEC's `stickys` row, and the reason SPEC gives for the row being gone is the
better argument for not moving it somewhere else: what that line said is now on the
screen itself. Which session is the sticky one is the mark its name carries in the
list and its box around the window it scrolls; whether a session is shown is the
window beside the list; and a sticky value shows up in the echo of every command it
fills (`$ send_message main "…"`).

Nothing is lost that a reader could not see: every session has a row in the list,
and a shown one has its own window beside it. The one thing the line carried that the
canvas does not is `block_size`, and that is a setting read back through
`set_render`, which is where a setting belongs.

## Client::command

This is the whole interface of the one-shot path (`gongga-cli send_message main hi`). There is no grammar here on purpose: a command line arrives tokenised by
the shell that started the process, so there is nothing left to decide. `/name`
is the *interactive* input line's syntax and prose is not a command; splitting a
string back into tokens only to re-derive an intention would let the two paths
disagree, and that disagreement is exactly what made `gongga-cli cancel` -- the
form the agent is told to use -- send the word "cancel" to the model as a chat
message.

## Client::message

The interactive input line decides that a line without `/` is prose; this is
what it asks for then. Assembled as tokens, never as a line: whatever the
operator typed -- quotes, `--`, backslashes, a whole paragraph -- is one
argument, and no parsing rule can turn part of it into a flag.

## Client::take_server_change

Nothing sets it any more: `connect_server` is the window's command, and the window
learns the new address from its own answer (`Done::Moved`). The accessor stays for the
one thing it still answers -- whether a move is pending -- so a front-end asks once.

## One-shot: no remembered address

`resolve_server` reads `--server`, then `GONGGA_SERVER`, then `[server] bind`. It used
to fall through to the address a `connect_server` had written into `cli.toml`, which is
how a one-shot command reached a server nobody named: exactly the client state this
crate no longer reads. A tool call has `GONGGA_SERVER` exported for it, so the agent
still reaches its own server.

## Client::submit_tokens

`echo` is only about what the operator sees: a command is echoed as the input
line showed it, a message as the prose it was. What goes on the wire has the
same shape either way.

Everything below is about *commands*: the catalog, the required arguments, the
wire. That a line without `/` is prose was decided before this, by whoever had an
input box -- and so was every sticky value it carries, which arrives already bound.

## Client::submit_tokens — offline

Offline, the catalog holds whatever the last greeting said -- and a client started
before its server has had no greeting at all. Falling through to "unknown command"
would then blame the operator for the server being down, so the line says what is
really wrong: `{head}: not connected to the server, nothing was sent`.

What a window runs itself is not in this catalog at all (`docs/notes/tui-command.md`),
so no client command reaches this path. What offline cannot do is parse a *server*
command, whose spec only a greeting carries.

## Client::submit_tokens — unknown command

A command line is not an input line: here a command is just its name. Two shapes
still arrive as something else --

    `/name ...`           the interactive input line's syntax, carried onto a
                          command line by a script or a model
    `C:/.../Git/name ...` the same thing after Git Bash rewrote the leading `/`
                          into a path, which happens before this program is even
                          started

-- and both are best answered with the line that was meant. "unknown command"
over a path nobody typed says nothing about the mistake, and that mistake is
easy to make: the agent is told to drive the harness with `gongga
<command>`.

The last separator is what finds the name in both shapes. Split by hand rather
than with `Path`, which does not call `\\` a separator on every platform.

## Client::submit_tokens — the rest of the line

Whatever followed the name, in either shape: inside the one argument when the
whole line was quoted, or in the remaining tokens when it was real argv.

## Client::submit_tokens — unknown flag

Passing an unknown flag through used to be harmless-looking and wrong: after the
session argument was renamed, `--name x` bound nothing and the command quietly applied
to a session the caller had not named. The spec is the only syntax this layer knows,
so an unknown flag is refused before anything is sent.

## Link

A front-end holds one of these but never sees a frame. Encoding and decoding
stay behind these methods, so "what does the wire look like" remains a question
only this crate answers.

## Link::goodbye

Worth nothing if it fails: the caller is already closing. Sending it anyway lets
the server drop this connection now instead of waiting out its idle timeout.

## main

The one entry point both binaries use for a command line: it sends `serve` to the
server mode before any runtime exists, and builds the runtime for everything else.
Returning the runtime from a function rather than from `#[tokio::main]` is what
lets `serve` own its own.

It knows nothing about which program called it: `serve` gets its home from the
command line and links *itself* into that home under the command's name
(`cli::server::place`), so the layer never has to be told which half of a program
it is.

## CLI, COMMAND, home_of

`CLI` is the word a program that merely *carries* the layer answers to
(`gongga-tui cli ...`), and `COMMAND` is the name a shell types -- one spelling
each, so the window, the link the server places and this paragraph cannot disagree.

`home_of` is `--home`, then `GONGGA_HOME`, then `~/.gongga`: the home a front-end
needs for the server it may start, and nothing else.

## client_home_of, endpoint_name

`client_home_of` is `--client-home`, then `GONGGA_CLIENT_HOME`, then
`~/.gongga-client`: the directory a *client* owns -- the rows it was given, and
nothing a server reads. It is separate from `home_of` on purpose: a server on another
machine has a home of its own, and even on one machine the two have different
lifetimes and different sizes. `docs/notes/tui-cache.md` is what lives there.

`endpoint_name` is the address this client would dial, resolved from its own
configuration (`--server`, the variable, what `connect_server` remembered, the config's
`bind`). It names a set of records, so two servers are two sets -- and it is known
before any server speaks, which is why nothing a window keeps depends on a greeting.

## run

Runs one command against the server, prints the events, exits.

`gongga-cli send_message main hi`. One command per process is the
whole interface: there is no `exec` to wrap it in, and nothing to configure
about the wait -- the command returns when the server answers, which for
`send_message` is the moment the message is accepted, not the moment the turn
finishes.

The argv is taken as it arrived. It is not turned back into a line first,
because the line grammar is the interactive front-end's: here a command is just
its name and its arguments, and there is no second meaning to fall into.

## run_once — Rejected

Rejected before it reached the server, so there is no event to print. The reason
still has to come out: "did not produce a command" would leave the caller
guessing whether the name or an argument was wrong.

## run_once — Offline

Cannot happen here -- this path only runs after a successful dial -- but a
silent `todo!()` would be worse than saying so.

## run_once — no deadline

What is being waited for is one command's acknowledgement, not the work it may
start. `send_message` returns as soon as the server has taken the message, and
the turn runs afterwards. A server that stops answering drops the connection at
its own idle timeout, which ends this wait with an error of its own.

## run_once — the event is the output

JSON lines survive `jq`, a model reading them, and a pipe; rendered text
survives none of those.

## Endpoint

Resolved once per process, for the reason `Connection::endpoint` gives: two
attempts of one reconnect must not mean two different servers.

An `Endpoint` carries an address and a key, and nothing about ownership: a dial is
a read. There used to be `Endpoint::owning`, which put the window's pid in the hello
so an unowned server would follow it -- and that made a server somebody had started
by hand stop when the first window closed. A server's lifetime is set by whoever
started it (`--parent`), so a client has nothing to say about it. See
`docs/notes/server-signal.md`.

## Client::draining, and the status line

"offline — reconnecting" is a claim about the *link*, and it was being made for a
server that was neither offline nor lost: a `cancel` drains for minutes, and while it
drained the server answered nothing, so the window said the only thing it could. The
greeting now states `draining`, and the line says "stopping — the server keeps
answering until its turns finish" while it is true. The two states are not the same
fact, and a reader deciding whether to close a window needs the difference.

The greeting alone was not enough: a window attached *before* the `cancel` learned
nothing, because `draining` only ever travelled in a hello. So the row a `cancel`
writes -- `events.kind = server_stop` -- is also the notice: it arrives on the same
bus as everything else, sets `draining`, and the status line is refreshed from that
state rather than from the sentence that happened to be the newest note. That is why
the line survives the session updates that follow, and why it says the state and not
the name: who asked is recorded in the row's payload, where a reader asks the log
for it.

## client_label

What this program calls itself in its hello: its own file name and its pid, e.g.
`gongga-tui/pid-1234` or `gongga-cli/pid-5678`. The program's name rather than the
crate's, because one function serves the window, the one-shot command and the
bundle's own `gongga`, and a record of who asked -- `server_stop`'s `payload.by` --
is only readable if it says which of them it was. The pid is what the operator then
looks up; the name is what makes the lookup worth doing.

## DialFailure::Fatal

The key is wrong, the address is malformed. Retrying would only hide the reason
behind an endless "reconnecting".

## DialFailure::NoListener

The port answered "nothing here" instead of saying nothing at all, and that is the
one dial failure starting a server is an answer to -- which is why it is its own
case rather than a `Temporary` with a different sentence. A front-end may report
it as news ("nothing is listening at 127.0.0.1:9700 -- starting one") instead of
as an error it will simply retry through.

## dial

Every call binds a new local port on purpose. The server routes datagrams by
peer address, so a link that died and came back on the same address would be
handed to the connection the server still remembers -- the dead one. A fresh
port makes the SYN unambiguously a new connection, which is also what the
server's own demultiplexer expects.

## dial — waiting for the greeting

The greeting is the first thing the server sends, so waiting for it is both the
handshake confirmation and the command catalog download.

A channel that ends before a greeting has two possible causes and the socket is
the only one that knows which: `ChannelStream::refused` says the address refused
a datagram, so there is no server there; anything else means a server was there
and hung up. They are different facts about the world -- one is answered by
starting a server, the other by looking at the one that is running -- so they are
reported differently even though both mean "no link".

## Greeting

Opaque on purpose: the greeting *is* a `ServerMessage`, and keeping it out of the
signature is what lets a front-end dial without linking against the protocol. It
is applied later rather than inside `dial` so that dialling borrows nothing but
its endpoint -- which is what lets the interactive loop keep dialling while it
still owns the screen.

## Greeting::apply

This *is* the resync, and there is no separate path -- the same message that
starts a session also hands back everything a fresh client needs, which is the
whole reason reconnecting is cheap.

## build_client

The catalog starts empty and is filled by the greeting, which is why a client can
exist before its server does -- and why, with no greeting yet, it knows no
command at all rather than a stale list of its own.

It reads nothing from the home it is given: `cli.toml` is the window's, so a one-shot
process cannot inherit a sticky session, a remembered address or a drawn set. The home
is there for one thing -- where `skills/` is read from.

## connect

One-shot mode only: there is no reconnect here, because one command that cannot
reach a server has nothing to wait for. An interactive client dials from inside
its own loop instead, so it can outlive any number of restarts.

Nothing is started here. SPEC gives the auto-start to the window, and a one-shot
command is what a script or a model runs: a script that silently brings a daemon
up on somebody's port is a surprise. `serve` is the command that starts one, and
the window calls it too (`cli::server`).

## server_of, server_address

Two ways to say where a client goes, neither of which needs a key:
`server_of` is the address a connection *would* use (the window needs it before a
server exists to mint its key), and `server_address` turns `--ip`/`--port` into
one. `server_address` defaults to `127.0.0.1` rather than the name `localhost`
because that is what `serve` binds: this platform resolves `localhost` to `::1`
first, and the two ends of one command line must agree without asking DNS.

## connect_elsewhere

`connect_server`, decided before a link exists -- it is the command that says
where a link would go. It saves the address first and dials second, then prints
one JSON object: the saved address outliving a refusal is the point, because the
next command will report the same refusal in its own words rather than dialing
somewhere else entirely.

## connect — the retry hint

One-shot mode's caller is usually a script or the model, so the one thing worth
adding to the transport's own words is the usual cause.

## Env

Injected rather than read directly so the precedence rules can be tested without
mutating global state. That is not tidiness: the harness exports `GONGGA_SERVER`
into every tool call's shell (it is how the agent reaches its own server), so a
test that reads the real environment passes or fails depending on who launched
it.

## a_response_line_names_its_session

Every part shows the tag, and the label follows it, so a reader scanning the log
can tell whose thinking, tool call, or answer a line is without the server's
session appearing anywhere else on the line.

## a_tool_answer_line_names_its_session

Both stdout and stderr carry the tag and stay separate streams, so a tool's
error output is attributable without being merged into its output.
