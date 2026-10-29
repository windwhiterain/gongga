# cli/src/bin/gongga.rs

## Why the command layer has a program of its own

`gongga` used to be the terminal's name: one binary that was headless when given a
command and a window when given none. That made the thing the agent types inside
its shell -- and every script, the evolve launcher and the bridge dial with -- a
build of the front-end, and a bare `gongga` in a pipe tried to draw a UI into a
pipe.

Now each program is one shape. This file is the command layer's own front-end:
argv in, the command's events out, no terminal in sight. The window is
`gongga-tui`, in the crate that depends on this one. The split is the same one the
libraries already drew, applied to the last place it had not been applied.

## A bare call lists what exists

With no command, `gongga` prints the catalog the server sent in its greeting --
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
derive, because one `Args` struct now serves two programs. `gongga --help` and
`gongga-tui --help` therefore describe the thing that was actually run: the usage
line, and the clap `about` line, are the caller's words. `Args` itself keeps no
`about` -- a default that neither binary prints would only drift.
