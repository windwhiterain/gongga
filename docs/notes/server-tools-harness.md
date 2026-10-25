# server/tools/harness.rs

## harness

The agent's own door into the harness. It takes a `command` and `arguments` and
calls `dispatch` directly, in the same process, so the agent drives the server it
lives in without spawning anything.

Before this, the only way was the CLI: `$GONGGA_CLI send_message ...`. That is a
loop through the network -- the agent runs inside the server, the CLI dials that
server over UDP, and the server answers a request it could have made itself. It
also made the agent depend on a binary being installed and reachable, and it cost
a process and a handshake per call.

What did *not* change: the CLI is still how scripts talk to the harness
(`server_script`, the evolve launcher, the skill scripts). Those are separate
processes; the tool is for the agent, which is not.

## The cycle, and `Weak`

`Dispatch` owns `Agent`, which owns the `ToolRegistry`, which owns this tool. The
tool therefore cannot hold an `Arc<Dispatch>` without a leak, so it holds a
`Weak` one, and `main` builds the dispatch with `Arc::new_cyclic`. The weak
reference is empty only while that closure runs, before anything can call it.

## Why the catalog is not baked into the tool

`parameters()` is static and synchronous, and the catalog is a filesystem scan of
the scripts directory. So the tool takes a free-form `command` and lists what
exists when called without one -- from `Dispatch::catalog`, the same list every
client gets in its greeting. Baking the names in here would recreate the second
list this refactor removed.

## Arguments

`arguments` is a JSON object keyed by argument name without dashes. Values are
stringified: `30` becomes `"30"`, a nested object becomes its JSON text, and
`null` means "not given" rather than the string `"null"` -- because `Args` is
`name -> string` for every command that already exists.

A leading `/` on the command name is accepted and stripped: the model has read
the interactive input syntax in the docs, and refusing it teaches nothing.

## Why the reply is capped

Over the wire, `net.rs` refuses a reply larger than one channel message. In
process there is no such limit, so a `get_messages --range 1:` over a long
transcript would land whole in the transcript and be paid for at every later
step. The tool caps instead, using the same head/tail-plus-file rule as `bash`,
so the full text stays readable.

## Client commands

`set_sticky_cmd_argument` and friends are refused by `dispatch` itself: they
change the caller's own state, and the agent is not the caller. That is a
property of the command surface, not a check the tool makes -- so it cannot be
forgotten here.
