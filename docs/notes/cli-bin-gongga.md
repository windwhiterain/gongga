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

## Naming itself

`cli::parse_argv(name, about)` takes the identity from the binary rather than the
derive, because one `Args` struct now serves two programs. `gongga-cli --help` and
`gongga-tui --help` therefore describe the thing that was actually run: the usage
line, and the clap `about` line, are the caller's words. `Args` itself keeps no
`about` -- a default that neither binary prints would only drift.
