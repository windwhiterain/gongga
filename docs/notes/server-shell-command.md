# server/shell_command.rs

## install

The agent drives its own server with a command line, so that command has to be
something its shell can find by name: `gongga send_message --session x`. The
server writes `<home>/bin/gongga` -- a two-line `exec` of the CLI's absolute path
-- and returns the `PATH` that makes that directory win the lookup.

Why a script in the home instead of putting the CLI's own directory on `PATH`:
the binary sits in `target/debug` during development, next to the server, the
bridge and every dll the build produced. Naming one directory for one command
keeps the agent's `PATH` from resolving whatever else happens to be in a build
output. It also means the name is chosen here rather than inherited from wherever
the binary was staged: `dev.sh` runs a *copy* of the server out of `.dev/`, so the
sibling of the running process is not where the CLI lives.

The script is not a convenience wrapper around `$GONGGA_CLI`. It carries the path
decided at startup, so a shell that drops the environment still runs the command
that was installed for it -- and the two cannot disagree about which binary is
being driven.

## Why the `PATH` is rebuilt rather than inherited

A value named `PATH` handed to Git Bash in the native shape (`C:\a;C:\b`) is
rewritten at the child's startup and comes out mangled -- measured here as
`C:/c/a` for a perfectly ordinary drive-qualified entry. So every entry is
translated with `msys_path`, joined with `:` the way a shell splits it, and
`bin` is put first. Empty entries are dropped: `PATH` treats one as the current
directory, and a shell that resolves commands out of whatever directory it happens
to be in is an accident waiting for a file named `gongga`.

That POSIX `PATH` is for a shell. A script interpreter launched with it would
lose its own native lookups, so `script::Scripts` is handed the three `GONGGA_*`
variables and the `PATH` it inherited untouched.

## Why `gongga` and not `cmd`

`cmd` is the shorter name and the one SPEC uses for a harness command, but in
Git Bash it already resolves to `/c/Windows/System32/cmd`. A `PATH` entry that
shadows the operating system's shell would be a surprise nobody asked for; the
name that wins is the one the harness actually owns.

## Executing a file a Windows program wrote

MSYS treats ordinary NTFS files as runnable, so the `chmod` a POSIX system needs
is compiled out here. Verified on this machine: a Rust process writes
`<home>/bin/gongga`, and the persistent shell, `bash -c`, and a `PATH` lookup all
exec it. The one thing that must not vary is the line ending -- a `#!/bin/sh` with
a trailing `\r` is a shebang for a program named `sh\r`.
