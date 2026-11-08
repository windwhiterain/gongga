# protocol/message.rs

## message

Everything on the wire is JSON inside the reliable channel's message frames. JSON
is not the smallest encoding available, but this protocol carries human-scale
events at human speed, and being able to read a datagram in a packet dump is
worth more than the bytes.

## ServerMessage::Unknown, and why events are not a closed set

`Event` is an internally tagged enum, and serde refuses a `t` it does not know.
That made adding a kind of event a *breaking* change: the server learned a new
word, and a front-end built before it failed to parse the frame and lost the
connection. Every addition was a lockstep upgrade of every client, which is why
"publish one event" used to look expensive.

`decode_server` now falls back. When the outer tag says `event` and the payload is
an object carrying a `t` of its own, the bytes are kept as `Unknown { event, why }`
and the client shows the kind instead of dying. Two shapes reach it:

* a `t` this build does not know — a newer server's event;
* a `t` this build knows, in a shape it cannot read — the server learned a
  required field first.

Both are the same situation from the reader's side, and neither is worth losing
the connection over.

The fallback stops where the frame stops being an event. A corrupt frame, an
outer tag nobody publishes, or an `event` payload with no tag of its own is still
an error: a tolerant decoder that accepts anything turns a bug into a silent gap.

The one-shot CLI prints an unknown event's bytes to stdout and never attributes it
to a session, because it cannot read which one it names. The window draws a
system line naming the kind. `crates/bridge` ignores it, which is right: a bridge
relays messages, and an event it cannot read is not one of them.

## ClientMessage::Hello

The server answers with `ServerMessage::Hello`, or `ServerMessage::Denied` when
`key` does not match the one the server was started with.

## ServerMessage::Hello — models

The greeting carries the models a request could run on here: names, and nothing else.
What a model *takes* travels on the argument that takes it (`ArgSpec.accepts`), because
that is the set the command will not refuse -- the roster is only how the server works
it out. An entry with no levels in it is not a hole in the roster: such a model takes
whatever it is sent, which is what an absent `PerModel` key says.

`#[serde(default)]`, so a greeting from a build without the table reads as a roster
that publishes none.

## ServerMessage::Event — the stream's cursor

Every event the bus publishes is numbered: one counter for the whole server, one
step per event, handed out where it is published and never changed. `seq` is that
number, and it is the answer to "which of these did a client miss" without anybody
having to guess — an event numbered above the one after the last it applied is a
hole, and nothing in this build invents the events inside it.

`None` is an event that is only *this connection's* answer: a command's own rows
and its `cmd_result`. Those are not part of the stream everybody else sees, so they
have no place in it; a client that treated them as news could not tell a repeat
from a gap.

The number restarts with the process, so it is only comparable with events from the
same `ServerInfo::pid` — which is why a greeting states the position its stream
starts from (`Hello::cursor`) rather than leaving it to be inferred.

## Event::Session, Event::SessionGone, Event::Transcript

A session changes as one row, and a deletion as one name. The whole table is sent
once, by the greeting, because that is the one moment a client has none; after that
it applies these two.

`Event::Transcript` is the other half of the same idea: every stored row is followed
by where the transcript now ends, so a client's own record of what it drew can be
compared with the server's without either side shipping the transcript. It is a
number about a *session*, never a line of one: no window draws it.

`seq` can go backwards. A failed step's rows are truncated, and the watermark says
so; a window that drew them is then stale in the other direction, which is why the
comparison is "different", not "behind".

## SessionInfo — transcript

`transcript` is the seq of the session's last stored row, read from the transcript
rather than counted, and it is what a greeting's session list is *for*: a client
compares it with its own journal's stamp. Equal is in sync; behind is a gap that
will be fetched when that window is next drawn.

`created_at` sits beside it because a name the server hands out again is a different
session whose rows start over: `seq` alone compares two unrelated transcripts
happily, and `(created_at, seq)` does not.

## SessionInfo — ended_at

`ended_at` is when the session's last turn ended, and it is the key the session list
is ordered by: the store lists the turn that ended last first, and a client puts a
delta row back in that order from the same number. `None` is a session that has not
finished a turn yet, which stands at its `created_at` -- a number every client
already has, so "newest first" needs no second field.

It is not `updated_at`, which is on the wire as well: a tuning change rewrites that
one, and a list ordered by it would move a session because its model was chosen.

## ResponsePart

The SPEC models a response as `{session, reasoning, toolcall, output}`; the
session name identifies the turn and the other three are interleaved deltas.

`Input` is the operator's own words: published when the row is written, and replayed
by a transcript read in exactly that shape. There is no local echo to lean on —
the window that sent nothing, a second window, and a session `send_message` reached
from a shell all draw the line from this event — so a row published after its own
`Event::Transcript` is a row nobody draws. See `docs/notes/server-agent.md`.

`Activity` is a fourth: a tool the *provider* ran on its own side -- a web
search, a web fetch, an image generation -- plus the sources it cited. It is not
`ToolCall`, because `ToolCall` means this harness called a tool and owes the
model an output; an activity has already been executed where it happened, and
nothing is owed. A front-end that ignored it would leave the operator unable to
see that the model went to the network at all.

## CmdArg

Sticky arguments are a client-side concern -- the window keeps them, fills them into
commands before sending, and never tells the server about them. The type lives here
only so the window's in-memory map and its on-disk state file agree on one shape.

## CompletionSource::Arguments

This is what a command takes when its value *is* an argument type, such as
`set_sticky <type>`: the kinds the published catalog declares are the only
sticky entries there are, so completing them is completing the settable ones.

## CompletionSource::Paths

For arguments that take a path rather than a word. The directory is the addressed
session's, not the client's: the server resolves the value in that session, and the
client is one process that may be standing anywhere.

## CompletionSource::OfNamedArgument

Complete this value the way arguments of the type the line names would be
completed: `set_sticky session <TAB>` offers sessions. The client needs this
because the shape of the value depends on which type it is going to fill.

## CompletionSource::Models

`Models` completes `provider/model_name` from the greeting's table. What each model
*takes* is not a source at all: it is `ArgSpec.accepts`, because that is what the
server's validation will accept, and a client that derived it from the roster
disagreed -- a level the roster maps (`medium` -> `high`) was offered by nobody and
dropped as one the model refuses.

## ArgSpec::accepts, ArgSpec::sticky_to

`accepts` is the closed set of values the argument takes, or `None` when it takes any.
It is published, not derived: the server's own validation is where the values come
from, so a front-end can answer "is this value accepted?" from the catalog alone. Two
shapes, because the roster is per model: `Same(values)`, and `PerModel` keyed by
`provider/model_name`, where an absent model accepts anything (the roster has no
opinion) and an empty list takes none (which is an answer too). `--cache_tier` carries
both facts: every model lists its declared tiers plus `NONE_VALUE`, and a model that
declares none therefore lists exactly `none`.

An argument with `accepts` publishes `CompletionSource::None`: one list per argument,
so a suggestion can never be read as a requirement. `--ctx_limit` keeps its ten
percentages as an offer while accepting any of a hundred.

`sticky_to` is SPEC's `(sticky to <kind>)` as data: `--thinking` and `--ctx_limit`
follow `model`, `--cwd` follows `remote`. The reader that keeps the entries applies
the rule without knowing a name.

## Event::Commands

A catalog is a snapshot of the roster, because an argument publishes the values the
roster lets through. Editing `providers.toml` therefore makes the greeting's catalog
stale, and the server republishes the whole of it beside `Event::Models` -- never a
delta, for the same reason the table is whole.

## Operation

The gestures a window offers, as API rather than as commands. `sticky_session`,
`render_session` and `delete_session` are what a click means; a window asks for
one of them and answers the two that are its own state itself, while the server
answers the one that is not -- and it answers it where it answers
`del_session`, so one deletion has one behavior however it was asked for.

The reason it is not a command line: the window would carry the command syntax,
and every change of that syntax would break the window. The reason it is not a
second way to say the same thing: `Operation::as_command` is where the two meet,
and the client-side operations never reach the server at all.

## ClientMessage::Operation

Beside `Command`, on the same wire: the message kind a window's gesture is
carried in. It carries an id like a command, so the answer is addressed the
same way, and the server answers it with a `CmdResult` -- the same answer, from
the same handler.

## ArgSpec::kind

A sticky entry keys on the argument's *type*, never its name: SPEC's `<session>`
is the kind `session` whether the flag spells `--name` (set_session,
send_message) or `--session` (get_messages, set_render_session). The kinds the
published catalog declares are the only entries there are -- the cmd surface
decides the sticky vocabulary, and a value lands only where a kind names it.
SPEC's Sticky section fills with an entry what a command omits: required
arguments outright, optional arguments as their initial value.

## ArgSpec::positional

SPEC's command lines bind arguments by position -- `interrupt_session <session>`
-- or by flag -- `set_session ... --cwd <path>`. The one field says which
spelling the line documents, and the declaration order in `args` is the binding
order, so a spec's list is its syntax: a nameless value guesses the parameter
right after the one the previous nameless value took, optional ones included.
A front-end parses and completes every server command from the catalog alone;
it keeps no list of its own, which is the "transparent wrapper, does not know
server cmd" half of the crate's contract. The client's own commands are the one
list it does carry (the published `client_commands`), because they have to work
before a server exists.
See `docs/notes/cli-catalog.md`.

## CommandSpec::target

Who runs a command, published rather than assumed. `server` means the server
runs it in-process; `server_script` means a script registered with the server
runs it on its behalf; `client` means the caller runs it against its own state
and the server never sees it.

Before this field existed, each front-end kept its own list of "the commands I
handle": the CLI had five client commands baked in, and anything else was
"unknown". Two lists meant two answers to "does this command exist", and the
client's list could not gain a command without a client rebuild. Now there is
one catalog, sent in the greeting, and a front-end dispatches on `target` -- so
a command registered by a script at runtime is complete, documented and
runnable with no client change at all.

An entry that omits the field reads as `server`: that is what every entry meant
before it existed, so an older catalog decodes with its meaning intact.

## connect_server, serve

The two client commands that name a process rather than the client's own state.
`connect_server` takes `--ip`/`--port`; `serve` takes `<port>` positionally, with
an optional `--home` -- a port is what the command is *about*, so it reads as
the sentence's object.

`serve` is the one command a front-end cannot run *for* you -- it takes the whole
process -- so the window answers it with where to type it, and the command line
runs it before any runtime exists (`cli::main`). Publishing it here anyway keeps
one catalog: what a client can run is what the greeting says, and a client
command that only one front-end can honour still belongs to the list.

## CompletionSource::StickyArguments and ::EventFilters

The server publishes the client's own commands, but the candidate values are the
client's state: which sticky arguments exist, which event filters exist. These
two sources say "complete against that list" without shipping the list, which is
the same arrangement as `Sessions` -- the server names where a value comes from,
and the side that owns it fills it in.

`set_render_session <session> <show>` completes the show word through a static
list, because `true` and `false` are the protocol's own words (`SHOW_VALUES`), not
the client's state. Its session completes through `Sessions` like every other
session argument.

## UsageEvent, Usage

SPEC's `context`, `cached`, `tps` and `ttft`, on the wire twice and for two
different readers: `SessionInfo::usage` is the last measurement, so a window that
dials now can draw a title before any turn happens, and `Event::Usage` is the same
numbers announced when a request completes.

`Usage` carries `cached_tokens`, not the percentage SPEC's title shows: the tokens
are what the provider reported, and a percent is a division the reader's side can
decide to spell.

`Usage` is split out of `UsageEvent` so the numbers can be carried without a
session name in the one place that does not need it -- the session is the field
next to it -- and `#[serde(flatten)]` keeps the wire shape flat. Sent per step,
not per turn, because the title says "latest": a step is a request, and a turn
that calls three tools made three of them.

The fields on `SessionInfo` are `#[serde(default)]`, so a client of a later build
reads a greeting from a server that has never heard of them instead of failing to
decode it: a window against an older server draws titles without numbers, which is
what that server measured.

## ClientInfo — no owner

`label` says who is talking, for a log line. Nothing else here is acted on: a hello
is a client saying it is reading, and a server's lifetime is decided by whoever
started it. There used to be an `owner` field -- the pid a window claimed an
unowned server with, so that server stopped when that window closed -- and it made
every server somebody started by hand die with the first window that connected.
`ServerInfo::owner` remains: it is the server *telling* a reader whose lifetime it
shares (`--parent`), which is a fact, not a claim.

## ClientInfo — session

`session` is the session this process was started from, read off `$GONGGA_SESSION` by
the client and sent as `#[serde(default)]` so a bridge, a window, and a server that
never heard of it all keep working. Only the process knows it: a command's `session`
argument names who it *acts on*, and `subscribe` is the one command where the two
questions are different -- a pattern names what to watch, the caller is who hears
about it.

## Event::name

The name a `subscribe <name> <pattern>` pattern is matched against: a log row's own `kind`, and
every other event's variant (`status`, `usage`, `sessions`, `log`, `cmd`, ...). Not a
second dispatch table -- a new row kind needs no entry, which is the same reason
`Record` carries its kind as data -- and the reason a pattern can spell a kind the
server has never heard of.

## Event::Status — `detail` and `outcome`

`detail` is the sentence a person reads beside the status word (`idle (cancelled)`),
and it is not a fact a program may parse: `"see log"` is a nudge, not a reason.
`outcome` is the machine's half, present exactly on the status that ends a turn, and
it is `TurnOutcome` -- `done`, `cancelled`, `failed`.

The distinction exists so `cli::Client` can ask the one question it has to answer
without reading prose: was this turn stopped by somebody? `Sink::stopped` is told
about a turn that ended, and a cancelled turn was ended by an operator asking for it,
so it is not announced on the desktop and its session is not marked as having
finished out of sight. `serde(default)`, so a status from a build without the field
reads as "outcome unknown" and is announced, which is what that build meant.

## SessionInfo — model, thinking_effort, context_limit

SPEC's `full` pane title lists them beside the measurements, so they travel with the
session rather than with a turn. They are what the session would send *now*: the model
and the notice share are resolved on the server, because only the server holds the
roster, while `thinking_effort` is the level the session was given -- the server stores
a model's default as a level rather than adding one to the title, so the title never
claims a level the request does not carry. All three are `#[serde(default)]`, so an
older server's greeting still decodes into a client that expects them.

`model` carries the whole model, `provider/model_name`, and not the provider alone: a
provider is only half of what a turn spends, and a title that named it without the
model would say whose account a step draws on but not what answered. `Quota` is where
the provider stands alone, because an allowance belongs to the account, not to the
model.

`cwd` was already here; it is part of the same title. It is where the session's own
shell says it is, not where the session was placed, because a tool reads that
directory and a front-end completing a value has to read the same one.

## Event::Models

The roster a client completes from, republished when the file changes. The whole table
travels, never a difference, and that is now the exception rather than the rule: a
*conversation* has a stamp and a store to fetch the rest from, while this roster is one
file a person edits, with neither -- so a reader applying differences would have to know
what it missed while it was away, and nothing on the wire says. It changes by hand,
rarely, and a greeting carries it anyway.

It is sent after the greeting, which already carries the same list, for the window that
stays connected while the file is edited. Nothing else in the client changes: the
`--model` and `--thinking` completions read the table, so the next completion offers what
the file says now.

## Quota

SPEC's `active_quota` row on the wire: the provider, the three window shares, a
credit, a balance, and the currency the balance is in.

Every figure is `Option`, and absent means "this provider did not report it" --
a month's share and a wallet balance are different facts, and a `0.0` default
would claim an allowance was spent. `note` is the provider's own words when it
flagged something (a spent window, an unavailable account) or the reason no
reading could be made at all, which is why the row can say why it is empty
instead of vanishing.

`Event::Quota` is a delta published at a step's end rather than a field of
`SessionInfo`: the row is about the *provider*, not a session, and a step that
names a new provider is what makes it "active".

## Head, Field and HeadKind

A block's introduction: what kind of thing it is, and the arguments it was given,
each with a label and a `summary` flag.

The split is one the protocol can make and the server must not. **The schema is
SPEC's**: `toolcall{tool_name·description, {name, background, detached, timeout} (bash)}`
says which arguments summarise a bash call, so that list is the protocol's own
vocabulary, not a rendering choice. What the server adds is the *verdict per field* --
this call set `name`, and `command` is detail -- because only it knows what the model
actually passed.

`description` is the one argument that is not a `Field`. It is what the call is *for*,
in the model's own words: the folded row can afford exactly one line of prose and this
is it, read on the name's row -- `bash·check the state`. As a field it would need a
label the title already said, and a field is one argument among several while this is
the row's subject. `Head::title` spells the `·` once, so the folded row and the open
item cannot disagree about where the name ends.

So the wire carries `Field { label, value, summary }` and never a table of names. A
front-end composes two rows from one list: the folded `abstract` is the fields marked
`summary`, the open item is all of them. Opening therefore costs no message -- both
rows were sent, once.

What the server must *not* send is a style. `HeadKind` is a closed semantic set that a
front-end maps to its own glyph and palette; a `color: "green"` on the wire would be one
program with opinions about another's screen, and the day two front-ends disagree the
wire grows a negotiation.

`value` is spelled where the server owns the unit -- a duration, a token count --
because a number and its unit are one fact. It is never a whole row's prose:
assembling the row is the front-end's, from the fields.

## ToolCallEvent, and a call's stream

A call is published as structure when its item is done: `call_id`, and a `Head` whose
fields are the arguments the model passed.

It also **streams**, as it always did: `ResponsePart::ToolCall` carries `bash(` then
the argument deltas then `)`, and every one of those chunks carries `call_id` (new on
`ResponseEvent`), so a front-end draws one block for the call from its first delta to
its verdict. The structured event that follows *fills that block* --
`Sink::rewrite`, then `Sink::relabel` -- instead of opening a second one.

That id is the whole reason it is on the wire: a streamed part is keyed by turn and
part, and a call is keyed by the id its result will carry. Without it the two halves of
one call became two blocks, one of them a bare `bash(`.

## REPLAY_TURN

A transcript row read back is an event, not a field of a result, and it is one of the
events the row was published as while it arrived: `response` for the parts an answer
has, `tool_call` for a call the model asked for, `tool_answer` for a tool's output,
`log` for what the harness told the reader.
Two things follow, and both are the point: delivery is **addressed** -- it goes to the
connection whose `get_messages` asked, the way `cmd` and `cmd_result` do, so a read
stays a read -- and the shape is **ordinary**, so every front-end renders it through
the path it already has for a streamed block. A replay that arrived as a special
envelope would be a second way to show a line, and the second way is where the two drift.

What a live stream has and a stored row does not is a turn that is still being written.
Turns count from 1, so `REPLAY_TURN` -- `0` -- is the mark a re-sent row carries: it
opens its own block instead of appending to the answer a running turn is writing under
the same part. It is also the only sign a front-end has that this part answers its own
read, which is what lets the bridge tell a read-back row from a live delta.

The row's `seq` stays off the wire: a reader advances by the `last_seq` the result
reports, and the range it asked for is what says which rows those were.

## BLOCK_SIZES

`--block_size` is a count of lines: any number is a valid answer, so the list here
is what is worth offering at a Tab, not the set that is allowed. The server's
`--context_limit` does the same thing with percentages, and for the same reason
-- a completion popup that offers nothing for a free-form number teaches the operator
nothing about what the number is for.

`0` is in the list first because it is the only word for "no cap". There is no mode
name to remember: the setting that replaced this one (`set_event_detail --mode`) had
to name a behaviour, and its two names differed only by whether a block folded.

## BOX_EDGES

`--box_edge` is a width in columns, so this list is what a Tab offers and not what is
allowed -- the same reasoning as `BLOCK_SIZES`. The starting value comes first, which is
`DEFAULT_BOX_EDGE` and not `0`: a box drawn edge to edge leaves a window with nothing to
grab under it, and `0` is a shape an operator picks rather than the absence of one.

## `draining`

`ServerInfo::draining` is true when the server is finishing a `cancel`: it is still
answering through the notice window, and it will not start another turn.
`serde(default)`, so a greeting from a build without it reads as "not draining". A
window already attached learns the same fact from the `server_stop` row the cancel
writes -- see `SERVER_STOP` and `docs/notes/client-lib.md`.

The field exists because both states look like silence on a wire. A `cancel` now
interrupts every turn and leaves, but the quarter-second acknowledgement window is
still a gap in which the socket answers nothing a window asked before -- a reader
who has to guess between "it is stopping" and "it is gone" guesses wrong, and the
guess it made was to print "reconnecting" for a cancel that was going fine.
`cli::Client` carries it into the status line: "stopping — its turns were
interrupted and it is leaving".

## `supervisor`

`ServerInfo::supervisor` is the watchdog a server runs under, when it runs under
one. It is `Option` and `serde(default)`, so a greeting from a server with no
watchdog reads exactly as it did before.

The field exists because the process a front-end spawned is no longer the process
answering: without it, "did I start this server" is not answerable, and a stop that
kills only the answering process is undone by a restart.
