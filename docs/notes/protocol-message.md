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

## CompletionSource::StickyArguments and ::EventFilters

The server publishes the client's own commands, but the candidate values are the
client's state: which sticky arguments exist, which event filters exist. These
two sources say "complete against that list" without shipping the list, which is
the same arrangement as `Sessions` -- the server names where a value comes from,
and the side that owns it fills it in.
