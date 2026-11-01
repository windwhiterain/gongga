# cli/src/bin/gongga-cli.rs

## Why the command layer has a program of its own

`gongga-cli` used to be the terminal's name: one binary that was headless when given a
command and a window when given none. This program is `gongga-cli` now -- the
crate's own name -- because the program, the package and the link in a home's `bin`
are one thing, and a reader should not have to learn a second spelling for it. That made the thing the agent types inside
its shell -- and every script, the evolve launcher and the bridge dial with -- a
build of the front-end, and a bare `gongga-cli` in a pipe tried to draw a UI into a
pipe.

Now each program is one shape. This file is the command layer's own front-end:
argv in, the command's events out, no terminal in sight. The window is
`gongga-tui`, in the crate that depends on this one. The split is the same one the
libraries already drew, applied to the last place it had not been applied.

## main belongs to the command layer

This file parses argv and hands it to `cli::main`, and does nothing else. The
entry point moved down because one of the commands -- `serve` -- has to be decided
before a runtime exists: the server builds the runtime it wants to be stopped in,
and a nested one would panic. Keeping that decision in this file would have meant
two binaries each knowing which command needs a runtime.

This program *is* the command layer, and it is also the name the layer is
installed under: whichever command layer started a server links itself into
`<home>/bin/gongga`, beside the `gongga-server` it started, so this file and that
link are the same layer (`tui-main.md` has the other half of that).

It carries no server. It used to: `crates/server` was a library this binary
included, which made every command -- and the window, and every test build --
link SQLite, reqwest and the whole agent loop to send one datagram. `serve` now
starts the program instead, and `gongga-cli` is 6 MB rather than 23.

## A bare call lists what exists

With no command, `gongga-cli` prints the catalog the server sent in its greeting --
one line per command, its description, the arguments it takes and whether each is
required, and a marker for the commands that run in the client rather than here.

The list is never a copy kept in this crate. A script can register a command, so a
front-end that named the built-in ones would hide the seventh; this is the same
rule that kept the deleted `harness` tool from baking names into its description.
The greeting is fetched anyway, because a command needs the link, so the listing
costs one connect and no new server command.

A caller that wants the machine-readable form asks for a command and reads its
`cmd_result`; this output is for whoever is standing at a shell, and it is what the
agent reads to learn what it may say next.

## `wait` reads the stream the connection already carries

A one-shot command answers as soon as its acknowledgement arrives -- `send_message`
returns in about 70 ms while the turn it started is still running. Before `wait`,
the only thing a script could do about that was sleep and look, and every interval
is a guess: too short wakes early, too long wastes the length of the turn.

`wait` needs nothing from the server. Every connection already receives every
event on the bus, so waiting is a read loop with a predicate:

```
wait --session NAME [--timeout S]      # that session stopped running
wait --kind K [--session NAME] [--timeout S]   # one log row of that kind
```

Three things are not incidental:

* **It answers from the state it was greeted with.** A turn that finished before
  this process connected left a `Status` nobody here heard, so a purely listening
  wait would hang out its timeout on something that already happened. The greeting
  carries every session's status, and the idle/error cases are decided from it
  before a single read. An unknown name is refused for the same reason: waiting
  15 minutes on a typo reads as "it never finished".
* **Idle and error are both the end.** A wait that only accepted idle would hang
  exactly when a turn failed. The exit code separates them: 0 idle, 1 error.
* **A timeout leaves with 124**, the code `timeout(1)` uses, so a shell can tell
  "gave up" from "it failed" without reading prose.

`--kind` waits on `Event::Record`, which exists because the agent's log writes now
publish. That is the server-side half: a row written without an announcement is a
row only a poller can find.

`wait` is a client command, and the window says so instead of running it: a window
already draws every session's events as they happen, so there is nothing left for
it to wait on.

## Naming itself

`cli::parse_argv(name, about)` takes the identity from the binary rather than the
derive, because one `Args` struct now serves two programs. `gongga-cli --help` and
`gongga-tui --help` therefore describe the thing that was actually run: the usage
line, and the clap `about` line, are the caller's words. `Args` itself keeps no
`about` -- a default that neither binary prints would only drift.
