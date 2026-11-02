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

## The supervisor, and the one exit code it needs

`main` forks before clap: with `GONGGA_SUPERVISED` absent it is the supervisor and
runs `supervise::watch`, which starts this same program with this same argv and
restarts it when it dies without saying why. `--help` and `--version` are answered
by the child path, so a question never pays for a process it does not use.
See `server-supervise.md`.

The one thing that had to change here is the error path: `gongga_server::main`
returning an error is now exit code 2 instead of anyhow's 1, which is the whole of
the vocabulary the supervisor needs to tell "refused to run, and said why" from
"died without saying anything".
