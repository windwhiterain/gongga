# server/main.rs

## main.rs

The backend as a program: `gongga-server` parses its flags into `Args` and hands
them to the library (`server-lib.md`), which owns the runtime. Nothing else is
here, and that is the point -- `evolve` replaces *this file*, so what a successor
is must be a program and not a mode of somebody else's.

`--bind` rather than `--port`: the address is what the server binds, and
`127.0.0.1:<port>` is a shorthand the command layer expands (`cli-server.md`). No
flags at all is a legitimate way to start it -- every value then comes from the
home's config, and that is how a staged successor is launched.

## From<Command> for Args

The command layer's `Command` is clap's, the library's `Args` is not: keeping clap
out of the library is what lets a test build an `Args` without a command line, and
keeping the two structs apart is what lets the binary's flags move without the
library noticing.
