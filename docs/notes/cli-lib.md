# cli/lib.rs

## lib.rs

This crate owns the command surface: it takes the catalog the server publishes,
keeps the sticky arguments, and turns one command's tokens into either a local
action or a message on the wire. Which is which is the published `target`, not a
list here -- so a command a script registered is handled by the same code path
as a built-in, and none of them needs a client rebuild.

It does not own the *input line*. Whether a line is a `/command` or prose for
the agent is the interactive front-end's decision (`Client::command` and
`Client::message` are the two things that decision can ask for), so the
one-shot path -- `gongga-cli send_message --message hi` -- never has to guess: its
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

The label says which part of an answer this is; the window says whose answer it is.
In a split area the window header already names the session, so the tag drops the
`[<session>] ` prefix -- keeping it would print the same name twice, one column
shorter. In the one window it stays, because nothing else says who is speaking.

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

## Sink::block_size and Sink::layout

Both default to a no-op: a front-end that shows every row in one window has to know
neither setting exists, and the one-shot sink must not start rendering. Both are
pushed on every `refresh`, which is how a window that attached after the client
learned it still hears about it.

The layout is announced (`Layout::Merged` or `Layout::Split`) rather than a `split`
flag, because the front-end has to know the names and their order anyway -- and a flag
would leave it inventing a naming rule the command layer already owns. A session filter
narrows the announced windows with the lines it narrows: splitting into three panes
while rendering one session is a layout nobody asked for, and a pane that shows nothing
is not a pane. The lines carry
their window either way: the announcement says what a frame shows, never what is held.

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

## Client::handle_tool_answer

Same rule, same shape: `[<session>] ` opens the line, and the tool's stdout and
stderr stay separate streams (`t:<call_id>:<stream>`) that each carry it. The
tool name is not in the prefix today; the answer arrives after the `tool: <name>`
line that the call itself rendered.

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

## Client::connected

Everything else the client knows about the server -- the catalog, the sessions,
the model -- is what the last greeting said. While this is false that knowledge
is stale by definition, and the status line says so rather than showing a model
number as if the server were still there.

## Echo

Both halves of the grammar reach the same code; the only difference the operator
should see is that a command looks like a command (`/set_session --session main`)
and a message looks like what they wrote. The wire never sees this distinction:
a message is `send_message --message <text>`.

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

Filters hide *lines*, not state. A hidden `Status` still updates the session list
and still calls `refresh`, and a hidden `Sessions` is not hidden at all -- it
carries no session to filter on and is how the client learns the names it
filters against.

The reason is the one thing a filter must not break: the operator narrows the log
to one session in order to read it, not to stop the client knowing what is
happening in the others. A status line that vanished *and* left the session list
stale would make the harness look dead.

The gate sits here rather than in `Sink` because a front-end has no idea which
events were dropped, and a sink that could drop them would have to re-derive the
same rules in every implementation.

`run_once` does not filter at all: a script gets the events it asked for, one
JSON object each, and a filtered `cmd_result` would leave it waiting for an
acknowledgement that was never printed.

## Client::run_event_ui

`set_event_ui`'s three arguments are three settings, so the command is a loop over
them rather than three commands with one name. Two rules make that legible:

- **Everything parses before anything saves.** `--block_size 9 --split sideways` is
  refused whole, because half an applied command is an events area nobody configured
  on purpose -- and the refusal does not report the setting it left alone.
- **The answer is the whole setting**, on a write as on a read: `block_size = 3
  split = false` in one line, because two answers to one command is an argument.

## Client::run_session_visibility

`show_session` and `hide_session` change what the events area draws, and nothing
else: they write `HiddenSessions`, they answer with the whole set, and they ask
for a frame. The blocks are untouched, so `show_session` is a view change and not
a re-read -- which is the same rule the split follows.

## Client::layout

Which panes exist, and -- since a session can be hidden while the area is merged
-- which window the one pane does *not* read. Both spellings of "not on screen"
come from the same set, so they cannot disagree: a session that is hidden is a
session with no pane and with no lines in the merged view.

## Client::settle_follow

SPEC's `set_session` carries a second sentence: it also sets the sticky `session`,
which is what keeps `send_message --message ...` from needing `--session` said
every time. It runs on the acknowledgement, never on the submission, because a
refused `set_session` must leave the client pointed where it was: following a
session that `--llm nosuch/model` refused to create is the kind of wrong that
reads as "the log stopped".

`del_session` follows nothing. Which session a command goes to is the sticky's to
say, and the SPEC sentence about it names only `set_session`.

## Client::submit_tokens

Nothing else is followed: naming a session writes no filter, and `show_session` /
`hide_session` are the only commands that change what the events area draws.

## Client::refresh

Deliberately has no "current session" in it. The only thing that ever stood
there was the sticky `session` argument, and printing it turned a persistant
argument into something that looked like a mode the UI was in. What is left are
facts about the server: its model and how many sessions exist -- and, when there
is no link, that there is no link, because a model name with no server behind it
is a lie the operator cannot see through.

## Client::refresh — the footer

The footer is the only place the client's own state is visible, which is why the
filters and the hidden sessions join the sticky arguments there: a line that never
appears has to be explained somewhere, or the operator concludes the server stopped
talking. Each half is omitted entirely when it is empty, so the common case stays
one line of text.

## Client::command

This is the whole interface of the one-shot path (`gongga-cli send_message --message
hi`). There is no grammar here on purpose: a command line arrives tokenised by
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

## Client::remember_server, Client::take_server_change

The client-side half of `connect_server`: save the address, report it, and set a
flag the front-end reads. A one-shot command reads it by never asking — it decided
`connect_server` before dialing — and the window reads it to drop the link and
retry, which is the only way a running window moves to another server.

The flag is taken rather than read, so a front-end that handles two submissions in
one pass cannot redial twice for one move.

## Client::submit_tokens

`echo` is only about what the operator sees: a command is echoed as the input
line showed it, a message as the prose it was. What goes on the wire has the
same shape either way.

Everything below is about *commands*: the catalog, sticky arguments, required
arguments, the wire. That a line without `/` is prose was decided before this,
by whoever had an input box.

## Client::remember_server, Client::take_server_change

The client-side half of `connect_server`: save the address, report it, and set a
flag the front-end reads. A one-shot command reads it by never asking — it decided
`connect_server` before dialing — and the window reads it to drop the link and
retry, which is the only way a running window moves to another server.

The flag is taken rather than read, so a front-end that handles two submissions in
one pass cannot redial twice for one move.

## Client::submit_tokens — offline

Offline, the catalog is whatever the last greeting said -- possibly nothing at
all, on a client started before its server. Falling through to "unknown command"
would then blame the operator for the server being down, so say what is really
wrong. Client commands keep working from the last greeting, because they never
leave this process.

## Client::remember_server, Client::take_server_change

The client-side half of `connect_server`: save the address, report it, and set a
flag the front-end reads. A one-shot command reads it by never asking — it decided
`connect_server` before dialing — and the window reads it to drop the link and
retry, which is the only way a running window moves to another server.

The flag is taken rather than read, so a front-end that handles two submissions in
one pass cannot redial twice for one move.

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

## Client::remember_server, Client::take_server_change

The client-side half of `connect_server`: save the address, report it, and set a
flag the front-end reads. A one-shot command reads it by never asking — it decided
`connect_server` before dialing — and the window reads it to drop the link and
retry, which is the only way a running window moves to another server.

The flag is taken rather than read, so a front-end that handles two submissions in
one pass cannot redial twice for one move.

## Client::submit_tokens — the rest of the line

Whatever followed the name, in either shape: inside the one argument when the
whole line was quoted, or in the remaining tokens when it was real argv.

## Client::remember_server, Client::take_server_change

The client-side half of `connect_server`: save the address, report it, and set a
flag the front-end reads. A one-shot command reads it by never asking — it decided
`connect_server` before dialing — and the window reads it to drop the link and
retry, which is the only way a running window moves to another server.

The flag is taken rather than read, so a front-end that handles two submissions in
one pass cannot redial twice for one move.

## Client::submit_tokens — unknown flag

Passing an unknown flag through used to be harmless-looking and wrong: after the
session argument was renamed, `--name x` filled nothing, a sticky `session`
filled the real one, and the command quietly applied to a session the caller had
not named. Checked before `fill` so only what was typed is judged -- sticky
arguments never introduce unknown names.

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

## run

Runs one command against the server, prints the events, exits.

`gongga-cli send_message --message hi --session main`. One command per process is the
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

## tests::argv

The command layer takes tokens, not a line, so this is what the tests below hand
it -- splitting here would be the thing being avoided.

## an_argument_that_contains_a_space_stays_one_argument

`gongga-cli send_message --message "two words"` has to reach the server as one
argument, not two. Nothing splits or rejoins the argv, so the only thing that
could go wrong is the tokenizer inside the command layer.

## a_leading_slash_is_refused_with_the_command_line_it_meant

`/` is the interactive input line's syntax. On a command line it is not a
command name, and answering "unknown command `"/send_message"`" would hide the
one fact worth knowing -- so the rejection spells the line out.

This is not hypothetical: the agent is told to drive the harness with
`gongga-cli <command>`, carried the TUI's `/` onto it, and on Windows Git Bash
turned the `/word` into a `C:/.../` path before the client ever saw it, so the
command quietly became a chat message.

## a_shell_rewritten_slash_is_recognised

Git Bash turns `/get_messages` into a path before the program is even started, so
what arrives is a path ending in the command's name -- with the rest of the line
attached when it was quoted as one argument. Both shapes have to come back as
the command line that was meant, or the operator is left with "unknown command"
over a path they never typed.

## the_command_layer_does_not_depend_on_a_front_end

The layering this crate exists for. A command layer that pulled in the front-end
would drag a terminal into every script and every test that only wants to send a
command, and the cycle would come back the moment the front-end needed a command
name.

## the_command_layer_does_not_depend_on_a_front_end — one program, no terminal

It ships exactly one program, and that program is the command line: argv in, the
command's events out. What it must not ship is a dependency on a front-end -- a
terminal dragged into every script and test that only wants to send a command. The
assertion therefore reads the manifest for the `gongga-cli` binary this crate owns,
matching a real section line so a comment cannot satisfy it.

## no_env

The real one is not empty -- the harness exports `GONGGA_SERVER`, `GONGGA_HOME`,
`GONGGA_SESSION` and a shell-shaped `PATH`, and never the key, which
stays in the home's `server.key` -- so every precedence test states its
environment explicitly.

## the_environment_beats_the_config_file

The harness sets the address and points at the home the key is read from for every
shell it starts, so this is the ordinary case, not an exotic one: the shell beats
the config file.

## client_with

The client is marked connected: these tests are about what a command means, and
an offline client refuses every server command before it gets that far (see
`submit_tokens`), which would make every one of them test the same thing.

## one_shot_sink_keeps_errors_and_nothing_else

One-shot mode renders nothing, but a rejected line has no event to fall back on,
so the reason has to survive somewhere the caller can read it.

## status_line_has_no_current_session

The status line states facts about the server, not which session the front-end
considers itself to be in.

## an_unknown_flag_is_refused_and_the_known_ones_are_listed

The refusal names the ones it does take. This is the guard against a renamed
argument: the old spelling must not be accepted and silently ignored.

## an_unknown_flag_is_refused_and_the_known_ones_are_listed — the old spelling

The pre-rename spelling is exactly the case that bit: it used to be passed
through, and a sticky `session` made it look like it worked.

## a_response_line_names_its_session

Every part shows the tag, and the label follows it, so a reader scanning the log
can tell whose thinking, tool call, or answer a line is without the server's
session appearing anywhere else on the line.

## a_tool_answer_line_names_its_session

Both stdout and stderr carry the tag and stay separate streams, so a tool's
error output is attributable without being merged into its output.

## a_response_key_is_unchanged_by_the_tag

Same guard on the response side: routing is not derived from the rendered
prefix.

## logs_and_commands_are_not_tagged

The negative of the rule, and the one that bites silently: a server log already
begins with `{name}: `, so tagging it would read as `[peer] peer: ...`. A
command is this client's own input and belongs to no session.
