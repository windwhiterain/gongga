# bundle/main.rs

## One file, three programs

`crates/bundle` is not a fourth thing to install: it is the head of a
distribution. `scripts/bundle.sh` builds the programs, appends them to this stub,
and the result is one file a person can copy, name `gongga`, and run.

    gongga (27 MB) = head (320 KB, no dependencies) + gongga-cli + gongga-server

The head is the only part that has to exist before anything is extracted, so it
has no dependencies at all -- not even `anyhow`: it must build on a machine that
has nothing.

## wanted_program

Two answers, in this order:

1. **the file name**, byte for byte like `<home>/bin/gongga-cli` decides the
   command layer (`tui-program.md`): a copy named `gongga-server.exe` is the
   server. The name wins because a link cannot carry an argument;
2. **the leading word `tui`** (or `window`), taken off before the program sees
   it. Without this, opening a window would mean copying the file under a second
   name -- true, and not something a person should be told. Verified:
   `./gongga tui --version` answers as `gongga-tui` with its own build.

Anything else is the command layer, which is what a bare `./gongga` is.

The bundle is the only thing that knows all three exist, and it is allowed to:
dispatching between them is its whole job. The command layer does not learn a
`tui` verb -- that would break `tui -> cli` never the reverse.

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

## Using it

```sh
./gongga --version        # this is gongga-cli, and here is the build
./gongga serve --port 9700
./gongga                  # the catalog, from that server
./gongga tui              # the window
```

Every run places all payloads in `$GONGGA_HOME/bin` first (all of them, because
`serve` looks for the server beside itself, and `tui` looks for nothing but its
own two halves). A file already holding those bytes is left alone, so the second
run costs a checksum per program and nothing else.
