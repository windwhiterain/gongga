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

## The shell's own tools are named, not inherited

`PATH` for a shell is not the server's `PATH` retyped: it is the home's `bin`,
then the shell's own tool directories, then what the server inherited.

The third of those was for a while the first two as well, and the result was a
shell that could not run `ls` -- `command not found`, while `find` resolved to
`C:\Windows\system32\find.exe`, a text search with none of GNU `find`'s flags.
An agent reads a shell that cannot `ls` as a broken shell, and the cause is not
the server's environment but the child's: Git Bash's coreutils live in
`<git>/usr/bin`, which a native `PATH` (`C:\Windows\system32`, `Git\cmd`,
`Git\bin`) never names. A terminal a person opens gets them from `/etc/profile`;
this shell is started `--norc --noprofile` and inherits nothing of the sort.

So `path` names them itself, as the shell spells them: an MSYS root is mounted at
`/`, so `/usr/bin` and `/bin` are right wherever Git is installed. They sit after
the home's `bin` -- the command layer still wins -- and before the inherited
entries, so that `find`, `sort` and `tar` mean the POSIX tools and not the
operating system's namesakes.

Each name appears once, first spelling wins. A handover hands the previous
shell's `PATH` on -- the successor is started by a shell, and inherits it -- so
the entries this adds would come back as inherited ones next time and the list
would grow by an entry per handover. It already had: two copies of the home's
`bin` and two of Git's `usr/bin` in one measured shell. Two spellings of one
directory are one name here, compared trimmed of a trailing separator and
case-folded, because on Windows they are the same directory.

Only for a shell. `native_path` is what a script's interpreter reads, and on Unix
nothing is added at all: there the tools a shell runs *are* the ones it inherited,
and an injected `/usr/bin` would put a distribution's copy ahead of whatever this
server was started with.

## Why `gongga-cli` and not `cmd`

`cmd` is the shorter name and the one SPEC uses for a harness command, but in
Git Bash it already resolves to `/c/Windows/System32/cmd`. A `PATH` entry that
shadows the operating system's shell would be a surprise nobody asked for; the
name that wins is the one the harness actually owns.

## ensure_command

The directory is only half of it: an entry on `PATH` that holds nothing is a
shell with no command. Before the split the server *was* `gongga`, so its own
`current_exe()` was the thing to link; now it is `gongga-server`, and the command
layer is a different program that the CLI installs right before spawning a
server (`cli-server.md`).

That leaves a hole with real cases behind it: `evolve`'s launcher `exec`s
`gongga-server` directly, `dev.sh server` stages it into `.dev/`, and a bare
`cargo run -p gongga-server` starts one too. None of them has `gongga-cli` beside
it, and a home first started that way had an empty `bin` -- an agent whose shell
cannot reach the server it is running on.

So the server asks the same three questions the CLI does, in the same order:
`GONGGA_CLI_BIN`, the sibling of its own executable, this home's `bin`. It
installs only when the file is *missing*: refreshing a stale one is the CLI's
job, because the CLI is the program that knows which build it is, and a stale
copy is visible anyway -- the handshake carries `BUILD` both ways
(`protocol-lib.md`).

Not finding one is a warning, not a refusal. Serving without a command still
serves; a server that refused to start would turn a missing file into an outage.
The warning names every place it looked, so the fix is in the message.

## GONGGA_CLI

Exported to every shell and script when the command was placed, as an absolute
path. `PATH` is enough for a shell, but a launcher or a script written by hand
should not have to trust a `PATH` that a shell may have rewritten -- and the
variable is what the skills already tell an agent to use.
