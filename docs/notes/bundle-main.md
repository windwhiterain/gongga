# bundle/main.rs

## One file, three programs

`crates/bundle` is not a fourth thing to install: it is the head of a
distribution. `scripts/bundle.sh` builds the programs, appends them to this stub,
and the result is one file a person can copy, name `gongga`, and run.

    gongga = head (no dependencies) + gongga-server + gongga-tui + gongga-runner

The command line is not one of them any more: it is `python/gongga_cli.py`, which
a server writes into `<home>/lib` and names in `<home>/bin` (`server-shell-command.md`).
A head that carried a copy would be asking with a build the operator may have
replaced -- and the layer moved once already, which is exactly why the head does
not know where it lives.

`gongga-runner` rides along because a remote session's shells need it on the
*other* machine, and this file is the thing that gets installed there: `ssh peer
gongga runner` is how a peer runs it (`server-remote.md`). It is an internal stdio
program, not a command a person is expected to type for their own work.

The head is the only part that has to exist before anything is extracted, so it
has no dependencies at all -- not even `anyhow`: it must build on a machine that
has nothing.

## wanted_program

Two answers, in this order:

1. **the file name**, byte for byte like `<home>/bin/gongga-tui` decides the
   program (`tui-program.md`): a copy named `gongga-server.exe` is the server. The
   name wins because a link cannot carry an argument;
2. **a leading word**: `tui` (or `window`), `server`, `runner`. It is taken off
   before the program sees it, so each is reachable from one file:

```
./gongga              the window
./gongga tui          the window, said out loud
./gongga server ...   the server program, with none of the layer around it
./gongga runner       the peer-side program of a remote session, on stdin and stdout
```

`runner` is the one word a person is not meant to type: it is what a peer's `ssh`
runs. It is a word here rather than a second installed name because the
distribution installs one file, and a remote's `runner` can still name any other
spelling the peer has (`.agents/notes/2026-11-01-remote-is-a-shell-endpoint.md`).

Anything else is the window, which is what a bare `./gongga` is.

`serve` is not one of those words: it is the *command layer's* command for
handing a console to a server, and it goes through the config, the pinned
content and the install pass. `server` is the program with none of that. The
difference is worth knowing; the flags are not, so `--port <p>` is accepted by
both and rewritten to `--bind 127.0.0.1:<p>` for the server.

The words are reserved by this file, which decides before it has a server to ask
for a catalog. A script that registered a command named `tui`, `window`, `server`
or `runner` would be unreachable through this file.

Each program answers `--help` with the words typed (`GONGGA_NAME`), so
`gongga server --help` says `Usage: gongga server [OPTIONS]`.

The bundle is the only thing that knows all three exist, and it is allowed to:
dispatching between them is its whole job. The command layer does not learn a
`tui` verb -- that would break `tui -> cli` never the reverse.

## take_over

`exec` on unix, inherited stdio and the child's exit code on Windows -- the same
rule `cli::server::serve` follows, and for the same reason: one command, one
console, one exit code (`client-server.md`).

## payload (the feature)

The programs are embedded at *compile* time: `build.rs` runs `cargo build -p
gongga-server -p gongga-tui` in a target directory of its own and writes an
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

Every program lands anyway, not only the one asked for: a window started from a
payload looks for `gongga-server` beside itself and in `<home>/bin`, so extracting
one alone would hand it a server it cannot find.

A file that already holds these bytes is left alone, and that is not an
optimisation: the head may be started while the program it carries is running,
and Windows refuses to replace a running executable. When the replacement cannot
happen within a few retries and the file already holds these bytes, the existing
copy *is* the answer.

The retries are there for the scanner, not for the lock: a freshly written 20 MB
executable is exactly what an on-access scanner opens, and Windows answers a
rename over one with "access denied" for a few tens of milliseconds.

## payload::retire

`<home>/payload/` is not this file's alone: `remote/pick.rs` puts a peer's pair for one
protocol in it, under the protocol's number. So a directory this file may retire is one
carrying its marker (`<dir>/.payload`, the stamp it was placed for), and the marker is
read **before** anything in it is asked to stop -- an unmarked directory is somebody
else's, and a `cancel` sent into it is not undoable. A directory placed before the marker
existed stays where it is; leaving a dead one is cheaper than stopping a live one.

A build this one replaces is asked to stop, then removed: a server through its own
`cancel`, which interrupts its turns and lets its background jobs go, so the
handover keeps what was started to outlive it.

Nothing else is touched. `gongga-tui` is a window somebody is typing in and a
command line of an older build may be halfway through, so neither is killed to free
a directory -- the directory simply stays, and the next install retires it once those
processes have exited. Killing the client was the first shape of this and it made
every upgrade close the reader's window.

One server is not asked to stop: the one a window was told to use. `--client_only`
says "there is a server, and it is not mine", and a window that starts no server
will not take the port -- so the retire leaves that server running and its
directory with it. Stopping it here is what made `gongga tui --client_only` a
window that waited for a server the bundle had just killed, on every upgrade.
`--client_only` exists so that a server upgrade does not break the client, and
this is where that promise is kept.

`--force` is the exception to "asked, not killed". It skips `ask_to_cancel` for
every retired directory and stops the server by the pid `running_in` found under
that directory, then waits for the pid to go away before `take_over` starts the
new build. `--client_only` still wins: a window that needs no port does not get
one freed for it. See the section below for what this costs.

## What this is not

A build of the programs into one image. `gongga` used to include the server as a
library and every command linked it -- 23 MB to send one datagram. One *file* on
the wire is a packaging question; one *image* is a link-time one, and the answer
to the first is an overlay, not a bigger binary.

## Using it

```sh
./gongga --version        # the build, as the window reports it
./gongga server --port 9700
./gongga tui              # the window
./gongga tui --force      # the window, replacing a build whose cancel is stuck
```

Every run places all payloads in `$GONGGA_HOME/payload/<stamp>/` first (all of
them, because `serve` looks for the server beside itself, and `tui` looks for
nothing but its own two halves). A file already holding those bytes is left alone,
so the second run costs a checksum per program and nothing else. A directory whose
server is still running is left where it is; the next install retires it.

`<home>/bin` is the *server's* business, not this file's: the program this one
takes over with installs itself and its command pair at startup
(`server-shell-command.md`). That is why one handover replaces the whole
distribution -- payload for all three programs, and the pair a shell types.

## There is no read-only run

`--version` is not a free question. The head places its payload and retires the
build it replaces *before* it takes over with the program that answers, and
`retire` stops a running server by asking it to `cancel`. Pointed at a live home,
"which build is this file" therefore stops that home's server -- every turn on it,
including the one that typed the command.

So a check names its own home: `GONGGA_HOME=./scratch-home ./gongga --version`.
`scripts/bundle.sh`'s smoke does exactly that, because a build must never place
programs in the operator's own install.

The one exception is a window that said `--client_only`: it starts no server, so
it needs no port, so the server that is running keeps it.

## Why a feature rather than "always"

`cargo bundle` is an alias for the one command (`.cargo/config.toml`):

    cargo bundle   ->  cargo build -p gongga-bundle --features payload

The build script does all the work; the feature only decides whether to run it.
Making it unconditional would mean every `cargo build`, `cargo check` and `cargo
test` in this workspace spawns a nested cargo that compiles the whole dependency
graph a second time into a second target directory -- 35 s cold, 1.8 GB. That is
a cost the inner loop pays hundreds of times a day and the installer pays once,
so it is opt-in and one alias away.

## --force: a build whose cancel cannot finish

    ./gongga tui --force

A server on an old build can still hold a turn forever: its `cancel` only raises a
boundary flag, and a turn stuck in a shell write or a model stream never reaches
that boundary. The port stays taken and the command line asking for the cancel
waits with no timeout, so a new build has placed its payload but never opens a
window. The current build interrupts every turn's token and leaves, so this is a
way out of a stuck *old* server, not of this build.

`--force` is the operator's way out of that loop. The head reads the flag before
it calls `retire`; the program parses argv only afterwards, so write the flag
*after* the program word (`gongga tui --force`, not `gongga --force tui`). It is a
global flag in `client::Args`, so the command line accepts it too, and
`check_flag_order` still requires it before a command name.

What it costs: `taskkill /T` on Windows takes the server's whole process tree
with it, including every background job the server was holding, where a `cancel`
would have let those jobs go. Messages already in the database are not lost; the
turns simply stop. Use it when the old build is known to be stuck, not as the
everyday upgrade path.

What it cannot reach: `retire` walks `<home>/payload/<stamp>` directories other
than this build's and stops only processes whose executable lives in one of them.
A server holding the port from `<home>/bin`, a build `target/`, another home or
another worktree is invisible here. Freeing one of those is a separate, explicit
step: find the port's owning pid and stop that pid by hand.

## The cancel the upgrade sends does not flash a window

`ask_to_cancel` starts this home's command line -- `<home>/bin/gongga-cli`, or the
`.cmd` beside it on Windows, run through `cmd` -- to stop the server of a retired
build. It asks the name a shell would type rather than a copy in the payload: the
layer is this home's Python, and a head carrying one would be asking with a build
the operator may have replaced.
The bundle is the head of a distribution and may be running with no console
of its own -- in a service, in a script, over ssh -- so the `cancel` would be
given a new visible console on the user's desktop. It carries `CREATE_NO_WINDOW`
like every other command this file starts. The bundle depends on nothing, so it
spells the flag itself; `crates/console` is the same answer for the crates that
may depend on something.
