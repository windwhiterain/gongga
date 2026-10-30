# server/shell_command.rs

## path

The agent drives its own server with a command line, so that command has to be
something its shell can find by name: `gongga send_message --session x`.

Who puts it there is the CLI's business, not this module's: `gongga` is the
program a person starts, and when nothing answers it starts the server
(`cli::server`). The server only has to make the directory win, so what is left
here is that directory, the `PATH` value that puts it first, and the program
`GONGGA_CLI` names.

Why a script in the home instead of putting the CLI's own directory on `PATH`:
the binary sits in `target/debug` during development, next to the server, the
bridge and every dll the build produced. Naming one directory for one command
keeps the agent's `PATH` from resolving whatever else happens to be in a build
output. It also keeps the server from having to know where that directory is --
it is always `<home>/bin`, whatever staged the binaries.

## program

The other half of the command: not where a *shell* finds it (`path`, above) but
which file `GONGGA_CLI` names. It is this process, `current_exe()` -- the program
that was started as `serve`, whose behaviour is the server's by construction, and
which is the only build on this machine that can be guaranteed to match it.

It is a program rather than the script in the home's `bin` because a script is
what a *shell* can run: `scripts/server_script_example.py` drives the harness with
`subprocess.run([os.environ["GONGGA_CLI"], ...])`, and that cannot execute a
`#!/bin/sh` file on Windows. `None` only when the operating system cannot name the
running process; the variable is then left out rather than pointing at nothing.

## Why the `PATH` is rebuilt rather than inherited

A value named `PATH` handed to Git Bash in the native shape (`C:\a;C:\b`) is
rewritten at the child's startup and comes out mangled -- measured here as
`C:/c/a` for a perfectly ordinary drive-qualified entry. So every entry is
translated with `msys_path`, joined with `:` the way a shell splits it, and
`bin` is put first. Empty entries are dropped: `PATH` treats one as the current
directory, and a shell that resolves commands out of whatever directory it happens
to be in is an accident waiting for a file named `gongga`.

That POSIX `PATH` is for a shell. A script interpreter launched with it would
lose its own native lookups, so `script::Scripts` is handed the `GONGGA_*`
variables and the `PATH` it inherited untouched.

## Why `gongga` and not `cmd`

`cmd` is the shorter name and the one SPEC uses for a harness command, but in
Git Bash it already resolves to `/c/Windows/System32/cmd`. A `PATH` entry that
shadows the operating system's shell would be a surprise nobody asked for; the
name that wins is the one the harness actually owns.
