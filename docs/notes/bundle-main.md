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

## payload (the feature)

The programs are embedded at *compile* time: `build.rs` runs `cargo build -p
gongga-cli -p gongga-server` in a target directory of its own and writes an
`include_bytes!` table. So the file `cargo build` produces is the whole
distribution, and so is the file `cargo install` installs:

    cargo install --git <repo> --features payload gongga-bundle

That is the point of the feature being opt-in: a workspace build must not build
three other crates it is not going to use, and only a release wants the payload.

Two details that are not optional:

- the nested `cargo` gets `CARGO_TARGET_DIR` of its own. The parent holds a lock
  on the directory it is using, and a child waiting for that lock deadlocks the
  build that started it;
- the programs are copied into `OUT_DIR` and embedded from there, not from the
  nested build's output. `include_bytes!` makes a file an *input* to this crate,
  and the nested build rewrites its own output every time it runs -- so embedding
  the output closed a loop: every build was dirty, and a no-op build cost 8 s;
- `build.rs` watches `crates/`, `Cargo.toml`, `Cargo.lock` and the git ref, not
  just itself. The ref matters because a commit touches nothing under `crates/`
  yet does change what the embedded programs report.
  Cargo only tracks this package's own sources, so without that an edit to
  `crates/cli` left a stale program embedded indefinitely -- which happened, and
  is why the first attempt at a message change appeared to do nothing;
- `--profile dev`, not `debug`: `PROFILE` is the directory name, and cargo
  reserves `debug` as a profile name. The directory is still `debug`.

Assembling one by appending after the image was the first shape and is gone: it
needed a python heredoc to write a table, and it meant the file cargo produced
could only say it carried nothing.

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

## Why a feature rather than "always"

`cargo bundle` is an alias for the one command (`.cargo/config.toml`):

    cargo bundle   ->  cargo build -p gongga-bundle --features payload

The build script does all the work; the feature only decides whether to run it.
Making it unconditional would mean every `cargo build`, `cargo check` and `cargo
test` in this workspace spawns a nested cargo that compiles the whole dependency
graph a second time into a second target directory -- 35 s cold, 1.8 GB. That is
a cost the inner loop pays hundreds of times a day and the installer pays once,
so it is opt-in and one alias away.
