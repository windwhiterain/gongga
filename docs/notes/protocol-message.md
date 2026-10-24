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
