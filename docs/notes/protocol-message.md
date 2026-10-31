# protocol/message.rs

## message

Everything on the wire is JSON inside the reliable channel's message frames. JSON
is not the smallest encoding available, but this protocol carries human-scale
events at human speed, and being able to read a datagram in a packet dump is
worth more than the bytes.

## ClientMessage::Hello

The server answers with `ServerMessage::Hello`, or `ServerMessage::Denied` when
`key` does not match the one the server was started with.

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

This is what a command takes when its value *is* an argument name, such as
`set_persistant_cmd_argument --name`.

## CompletionSource::Paths

For arguments that take a path rather than a word.

## CompletionSource::OfNamedArgument

Complete this value the way the argument named on the same line under `--name`
would be completed: `--name session --value <TAB>` offers sessions. The client
needs this because the shape of the value depends on which argument it is going
to fill.

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
`connect_server` takes `--ip`/`--port`, `serve` takes `--port`.

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

`set_render_session --mode` completes through a static list, because the three modes
are the protocol's own words (`RENDER_MODES`), not the client's state. Its
`--session` completes through `Sessions` like every other session argument.

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

## SessionInfo — model, thinking_effort, context_limit

SPEC's `full` pane title lists them beside the measurements, so they travel with the
session rather than with a turn. They are what the session would send *now*: a session
that never named a level runs on the model's own default, and one that never named a
share is noticed at the server's -- resolved on the server, because only the server
holds the roster. All three are `#[serde(default)]`, so an older server's greeting
still decodes into a client that expects them.

`model` carries the whole model, `provider/model_name`, and not the provider alone: a
provider is only half of what a turn spends, and a title that named it without the
model would say whose account a step draws on but not what answered. `Quota` is where
the provider stands alone, because an allowance belongs to the account, not to the
model.

`cwd` was already here; it is part of the same title.

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

## REPLAY_TURN

A transcript row read back is an event, not a field of a result, and it is one of the
events the row was published as while it arrived: `response` for the parts an answer
has, `tool_answer` for a tool's output, `log` for what the harness told the reader.
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
