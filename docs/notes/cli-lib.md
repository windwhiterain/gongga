# cli/lib.rs

## lib.rs

This crate owns the command surface: it knows the catalog the server publishes,
keeps the sticky arguments, and turns one command's tokens into either a local
action or a message on the wire.

It does not own the *input line*. Whether a line is a `/command` or prose for
the agent is the interactive front-end's decision (`Client::command` and
`Client::message` are the two things that decision can ask for), so the
one-shot path -- `gongga send_message --message hi` -- never has to guess: its
argv is already tokens.

It knows nothing about terminals. That is the point of the split: anything that
can present `Sink` events can drive it, and the interactive client is one such
thing (the `gongga` binary in `crates/tui`). Nothing here names a rendering
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

clap hands an external subcommand every token after it, so `gongga send_message
--server 1.2.3.4:1` would take `--server` as one of the command's own arguments
and then quietly dial the default server.

## Sub::Cmd

Anything but a flag is the command itself, because what the client can run is
decided by the server and only known after the handshake: there is no fixed list
of subcommands to declare here.

`gongga set_session --name main`. One command per process, the answer on stdout
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

The prefix is the block's first row, so a block long enough to outgrow the window
shows its newest rows without the tag. That is the simple model's cost: it keeps
one block one line and reserves space for nobody, at the price of the label
scrolling off with the head while a single block streams.

## Client::handle_response

Who is speaking belongs on that prefix, not in the text: a session tag in the
text would be repeated by every delta. The prefix is `[<session>] ` followed by
the part's own label (`thinking: `, `tool: `, or nothing for output), which is
the same shape `Event::Status` already uses for `[<session>] idle`. The stream
key keeps the session for routing (`r:<session>:<turn>:<part>`) and does not
depend on the tag being shown.

Shown for a single session too. The tag costs one line start per stream, while
turning it on by session count would change how the lines of a session already
on screen look the moment another session is created -- and a prefix must stay
fixed for the life of its key.

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

## Client::follow_session_command

The two SPEC rules about `set_session` and `del_session` are one behaviour: the
session you name is the one this client follows. Naming a session sets both the
sticky `session` (so the argument can be omitted) and its event filter (so the
log is that session's), because those are the same intention spelled twice.

`del_session` only clears them when it deletes the session they point at.
Deleting some other session must not silently retarget a client that is watching
one, and neither must a failed delete -- hence the equality test rather than an
unconditional clear.

Neither runs at submission time. `set_session --llm nosuch/model` is refused, and
a client that had already followed it would be watching a session that does not
exist -- the kind of wrong that reads as "the log stopped". So the command is
remembered in `follows` (id, name, session) and `settle_follow` applies it when
its `cmd_result` says `ok`. That is one extra piece of state, and it is the reason
`run_once` now feeds events through `handle_event`: a one-shot `set_session` gets
the same acknowledgement, and would otherwise never follow at all.

## Client::refresh

Deliberately has no "current session" in it. The only thing that ever stood
there was the sticky `session` argument, and printing it turned a persistant
argument into something that looked like a mode the UI was in. What is left are
facts about the server: its model and how many sessions exist -- and, when there
is no link, that there is no link, because a model name with no server behind it
is a lie the operator cannot see through.

## Client::refresh — the footer

The footer is the only place the client's own state is visible, which is why the
filters join the sticky arguments there: a line that never appears has to be
explained somewhere, or the operator concludes the server stopped talking. The
filters half is omitted entirely when there are none, so the common case stays
one line of text.

## Client::command

This is the whole interface of the one-shot path (`gongga send_message --message
hi`). There is no grammar here on purpose: a command line arrives tokenised by
the shell that started the process, so there is nothing left to decide. `/name`
is the *interactive* input line's syntax and prose is not a command; splitting a
string back into tokens only to re-derive an intention would let the two paths
disagree, and that disagreement is exactly what made `$GONGGA_CLI cancel` -- the
form the agent is told to use -- send the word "cancel" to the model as a chat
message.

## Client::message

The interactive input line decides that a line without `/` is prose; this is
what it asks for then. Assembled as tokens, never as a line: whatever the
operator typed -- quotes, `--`, backslashes, a whole paragraph -- is one
argument, and no parsing rule can turn part of it into a flag.

## Client::submit_tokens

`echo` is only about what the operator sees: a command is echoed as the input
line showed it, a message as the prose it was. What goes on the wire has the
same shape either way.

Everything below is about *commands*: the catalog, sticky arguments, required
arguments, the wire. That a line without `/` is prose was decided before this,
by whoever had an input box.

## Client::submit_tokens — offline

Offline, the server's half of the catalog is whatever the last greeting said --
possibly nothing at all, on a client started before its server. Falling through
to "unknown command" would then blame the operator for the server being down, so
say what is really wrong. Client commands are unaffected: they never leave this
process.

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
easy to make: the agent is told to drive the harness with `$GONGGA_CLI
<command>`.

The last separator is what finds the name in both shapes. Split by hand rather
than with `Path`, which does not call `\\` a separator on every platform.

## Client::submit_tokens — the rest of the line

Whatever followed the name, in either shape: inside the one argument when the
whole line was quoted, or in the remaining tokens when it was real argv.

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

## run

Runs one command against the server, prints the events, exits.

`gongga send_message --message hi --session main`. One command per process is the
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

## dial

Every call binds a new local port on purpose. The server routes datagrams by
peer address, so a link that died and came back on the same address would be
handed to the connection the server still remembers -- the dead one. A fresh
port makes the SYN unambiguously a new connection, which is also what the
server's own demultiplexer expects.

## dial — waiting for the greeting

The greeting is the first thing the server sends, so waiting for it is both the
handshake confirmation and the command catalog download.

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

The catalog starts with only the client's half; the server's half arrives with
the greeting, which is why a client can exist before its server does.

## connect

One-shot mode only: there is no reconnect here, because one command that cannot
reach a server has nothing to wait for. An interactive client dials from inside
its own loop instead, so it can outlive any number of restarts.

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

`gongga send_message --message "two words"` has to reach the server as one
argument, not two. Nothing splits or rejoins the argv, so the only thing that
could go wrong is the tokenizer inside the command layer.

## a_leading_slash_is_refused_with_the_command_line_it_meant

`/` is the interactive input line's syntax. On a command line it is not a
command name, and answering "unknown command `"/send_message"`" would hide the
one fact worth knowing -- so the rejection spells the line out.

This is not hypothetical: the agent is told to drive the harness with
`$GONGGA_CLI <command>`, carried the TUI's `/` onto it, and on Windows Git Bash
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

## the_command_layer_does_not_depend_on_a_front_end — no binary

And it ships no program of its own: the binary is the composition root, which
lives where both halves are available. The *comment* in the manifest says the
same thing, so match a real section header rather than the substring.

## no_env

The real one is not empty -- the harness exports GONGGA_SERVER/GONGGA_KEY so the
agent can drive itself -- so every precedence test states its environment
explicitly.

## the_environment_beats_the_config_file

The harness sets both of these for every tool call, so this is the ordinary
case, not an exotic one: the shell beats the config file.

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
