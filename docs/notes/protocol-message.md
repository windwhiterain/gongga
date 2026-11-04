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

The greeting carries the models a request could run on here, each with the levels the
roster publishes for it, and that is the whole of what `--model` and `--thinking`
complete from. They travel together because they are one question asked twice: the
list of models is the same list, and a model and its levels cannot say different
things if there is only one of each.

`levels` is empty for a model the roster says nothing about, and that is a fact rather
than a hole: such a model takes whatever it is sent. `#[serde(default)]`, so a
greeting from a build without the table reads as a roster that publishes none.

## ResponsePart

The SPEC models a response as `{session, reasoning, toolcall, output}`; the
session name identifies the turn and the other three are interleaved deltas.

`Activity` is a fourth: a tool the *provider* ran on its own side -- a web
search, a web fetch, an image generation -- plus the sources it cited. It is not
`ToolCall`, because `ToolCall` means this harness called a tool and owes the
model an output; an activity has already been executed where it happened, and
nothing is owed. A front-end that ignored it would leave the operator unable to
see that the model went to the network at all.

## CmdArg

Sticky arguments are a client-side concern -- the CLI keeps them, fills them into
commands before sending, and never tells the server about them. The type lives
here only so both the CLI's in-memory map and its on-disk state file agree on one
shape.

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

## CompletionSource::Models and ::ModelLevels

`Models` completes `provider/model_name` from the greeting's table. `ModelLevels`
completes what the model *that line runs on* takes: the model it names, else the
client's sticky one, which is the value `set_session` would fill in. Neither is
`Static`: the list is a fact about this machine's roster, and the client holds no copy
of it.

An empty level list is not an empty popup. A model the roster does not describe takes
any spelling, so the client offers every level some model here publishes -- refusing to
guess would be the client stating an opinion the roster never had. `THINKING_DEFAULT`
is appended in both cases, because naming no level is always allowed.

## Operation

The gestures a window offers, as API rather than as commands. `sticky_session`,
`render_session` and `delete_session` are what a click means; a window asks for
one of them and the client answers the two that are its own state, while the
server answers the one that is not -- and it answers it where it answers
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

## ArgSpec::kind and ::bundle

A sticky entry keys on the argument's *type*, never its name: SPEC's `<session>`
is the kind `session` whether the flag spells `--name` (set_session,
send_message) or `--session` (get_messages, set_render_session). The kinds the
published catalog declares are the only entries there are -- the cmd surface
decides the sticky vocabulary, and a value lands only where a kind names it.
`bundle` groups arguments that fill as a unit: some members typed, sticky leaves
the rest alone; none typed, every member an entry holds fills.

## ArgSpec::positional

SPEC's command lines bind arguments by position -- `interrupt_session <session>`
-- or by flag -- `set_session ... --cwd <path>`. The one field says which
spelling the line documents, and the declaration order in `args` is the binding
order, so a spec's list is its syntax. Every non-optional argument may also be
spelled by order alone; an optional one needs its flag. A front-end parses and
completes every command from the catalog alone; it keeps no list of its own,
which is the "transparent wrapper, does not know server cmd" half of the crate's
contract.

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
answering, and it will not start another turn. `serde(default)`, so a greeting from a
build without it reads as "not draining".

The field exists because both states look like silence on a wire. A `cancel` hands every
running turn the same token, and the drain that follows can run for minutes while the
socket answers nothing a window asked before -- a reader who has to guess between "it is
stopping" and "it is gone" guesses wrong, and the guess it made was to print
"reconnecting" for a whole cancel that was going fine. `cli::Client` carries it into the
status line: "stopping — the server keeps answering until its turns finish".

## `supervisor`

`ServerInfo::supervisor` is the watchdog a server runs under, when it runs under
one. It is `Option` and `serde(default)`, so a greeting from a server with no
watchdog reads exactly as it did before.

The field exists because the process a front-end spawned is no longer the process
answering: without it, "did I start this server" is not answerable, and a stop that
kills only the answering process is undone by a restart.
