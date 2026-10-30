# server/shell_command.rs

## path

The agent drives its own server with a command line, so that command has to be
something its shell can find by name: `gongga-cli send_message --session x`.

Who puts it there is the CLI's business, not this module's: the server links
itself into `<home>/bin` under that name as it comes up (`cli::server::place`).
This module only makes the directory win, in the shape the child will read it.

Why a directory in the home instead of the program's own directory on `PATH`: the
binary sits in `target/debug` during development, next to the server, the bridge
and every dll the build produced. Naming one directory for one command keeps the
agent's `PATH` from resolving whatever else happens to be in a build output. It
also keeps the server from having to know where that directory is -- it is always
`<home>/bin`, whatever staged the binaries.

## path, native_path

The same directory, two spellings, because there are two kinds of child: a shell
reads `PATH` split on `:` with `/c/...` entries, and a script's interpreter reads
the platform's own `;` and `C:\...`. Both are built from one list, so `gongga-cli` is
one file whichever of them looks it up.

## Why one name is enough

The name in that directory is not a script, it is the program itself under
`gongga-cli`'s name (`cli-server.md`). That is what makes both spellings useful: bash
resolves `gongga-cli` to `gongga-cli.exe` on its own, and `subprocess.run(["gongga-cli"])`
from a script starts the same file. Measured both ways on Windows, where the
`#!/bin/sh` name this replaced had failed with `WinError 2` -- Windows appends
`.exe` and nothing else when a command has no extension.

A per-platform launcher (`gongga.cmd` beside a `gongga-cli` script) does not get
there: a native process has to spell `.cmd`, and a shell looks for `gongga-cli`. One
name both can run has to be the program, which is why the link is what this
module's `PATH` is for.

## Why the `PATH` is rebuilt rather than inherited

A value named `PATH` handed to Git Bash in the native shape (`C:\a;C:\b`) is
rewritten at the child's startup and comes out mangled -- measured here as
`C:/c/a` for a perfectly ordinary drive-qualified entry. So every entry is
translated with `msys_path`, joined with `:` the way a shell splits it, and
`bin` is put first. Empty entries are dropped: `PATH` treats one as the current
directory, and a shell that resolves commands out of whatever directory it happens
to be in is an accident waiting for a file named `gongga-cli`.

That POSIX `PATH` is for a shell. A script interpreter launched with it would
lose its own native lookups, so `script::Scripts` is handed the `GONGGA_*`
variables and the native `PATH` -- the same directory, spelled the way it reads.

## Why `gongga-cli` and not `cmd`

`cmd` is the shorter name and the one SPEC uses for a harness command, but in
Git Bash it already resolves to `/c/Windows/System32/cmd`. A `PATH` entry that
shadows the operating system's shell would be a surprise nobody asked for; the
name that wins is the one the harness actually owns.
