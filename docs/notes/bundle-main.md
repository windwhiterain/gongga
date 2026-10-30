# bundle/main.rs

## One file, three programs

`crates/bundle` is not a fourth thing to install: it is the head of a
distribution. `scripts/bundle.sh` builds the programs, appends them to this stub,
and the result is one file a person can copy, name `gongga`, and run.

    gongga (27 MB) = head (320 KB, no dependencies) + gongga-cli + gongga-server

The head is the only part that has to exist before anything is extracted, so it
has no dependencies at all -- not even `anyhow`: it must build on a machine that
has nothing.

## program_name

The name this file was started under decides which program it becomes, byte for
byte like `<home>/bin/gongga-cli` decides the command layer
(`tui-program.md`). A copy named `gongga-server.exe` is a server; anything else
is the command line. No flag, because the shell that types the name has nowhere
to put one.

## take_over

`exec` on unix, inherited stdio and the child's exit code on Windows -- the same
rule `cli::server::serve` follows, and for the same reason: one command, one
console, one exit code (`cli-server.md`).

## payload

The table is the *last* thing in the file: `[footer_len: u64][MAGIC: 16]`, and
before it a count and, per program, its name, offset, length and an FNV-1a
checksum. Reading 24 bytes from the end is enough to know whether a file carries
programs at all, and a plain executable cannot be mistaken for one.

Appending after a PE image is what every self-extracting archive does: the loader
ignores the overlay, and on unix nothing looks past what the headers describe.

## payload::extract

Every program lands anyway, not only the one asked for: `gongga-cli serve` looks
for `gongga-server` beside itself and in `<home>/bin`, so extracting one alone
would hand it a server it cannot find.

A file that already holds these bytes is left alone, and that is not an
optimisation: the head may be started while the program it carries is running,
and Windows refuses to replace a running executable. When the replacement cannot
happen within a few retries and the file already holds these bytes, the existing
copy *is* the answer.

The retries are there for the scanner, not for the lock: a freshly written 20 MB
executable is exactly what an on-access scanner opens, and Windows answers a
rename over one with "access denied" for a few tens of milliseconds.

## What this is not

A build of the programs into one image. `gongga` used to include the server as a
library and every command linked it -- 23 MB to send one datagram. One *file* on
the wire is a packaging question; one *image* is a link-time one, and the answer
to the first is an overlay, not a bigger binary.
