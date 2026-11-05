# server/lib.rs

## Args

The whole of what a server can be told. `Default` is the empty command line, and
that is what `gongga-cli serve` passes on: it names a home and a port, and every
other value comes from that home's config.

The database has no path of its own: it is `<home>/gongga.db`, and no flag or
config key names it elsewhere. A throwaway run is a throwaway *home*, which moves
the database, the log, the blob directory, the job records and the instance lock
together. They used to be movable apart, and that is how a scratch server ended up
driving the operator's real database: the lock is per-home and the database was
per-config, so the two could disagree, and nothing noticed a second writer on one
file.

## Args::home

The home directory: the database, config, logs, detached-job records and the
instance lock all live under it. Overrides `[server] home`, which overrides
`$GONGGA_HOME`.

## run (the command an agent types)

The server does not look for the CLI, and does not need to: `current_exe()` is
`gongga-server`, which is not what a shell should type. What its children get is
`<home>/bin/gongga`, placed there by the command layer that started this server
(`link-server.md`) -- the command layer is a program of its own now, so the
directory is the only thing the two have in common.

One name, two `PATH` shapes. `<home>/bin` goes first in both: a shell reads a
POSIX `PATH` and types `gongga-cli`; a script's interpreter reads the native one and
starts `gongga-cli` -- which is the command layer, linked into that directory. Neither
needs a second variable, and neither can end up driving a different build than the
server they are talking to.

## main

The entry point as a function: `gongga-server`'s `main.rs` parses its flags into an
`Args` and calls this, and it owns the runtime for the life of the server.

The runtime is built here rather than by `#[tokio::main]` so that the drop
happens at a point where `run` has already closed every shell. The drop still
waits, with no deadline -- but it now waits for something that ends, which is the
difference between waiting and hanging.

The bug this replaces: every blocking task had to drain before the process could
go, one of them was the reaper for a persistent shell, and a persistent shell
never exits on its own. After a session had used `bash`, the process sat there
having already logged `server stopped`. Putting a 2s deadline on the drain hid
that instead of fixing it; what fixes it is ending the shells in `run`, after
which there is nothing left for the drain to wait on.

## run (flag overrides)

The simplest way to make the flags win without threading overrides through the
loader is to promote them to the same environment variables the loader already
consults.

`SAFETY`: this runs before any thread is spawned, so the process is effectively
single-threaded and no other code can observe these writes.

## run (install, then read again)

The config is loaded twice: once to learn the home, which is what the lock and the
install both need, and once after the install, which is what everything else runs
on. A value from the first read is the loader's guess at a file that had not been
placed yet.

## run (instance lock)

Before touching the database: two servers on one home would each resume the same
pending turn and drive the same session twice.

## run (shell env)

Detached jobs live next to the database: they outlive this process, so their logs
have to outlive it too.

How the model drives itself, which is what this vec is for. `bash` runs inside
this process's children, so the server address, the command that speaks to it and
the session it belongs to have to be handed down rather than discovered: without
them an agent that wants to cancel or restart has no better option than killing
its own server from outside. `GONGGA_HOME` and `GONGGA_SERVER` are exactly the two
fallbacks the CLI reads, so a command needs no flags of its own. `PATH` is what
makes `gongga-cli` resolve, in the shape each child can read
(`server-shell-command.md`); `GONGGA_SESSION`, `GONGGA_MODEL` and `GONGGA_REMOTE`
are added where a session's shells are made, because those facts belong to a
session and not to the server: a shell cannot learn its own name, model or
machine from the outside, and a script places a child from them instead of a
flagless `set_session`, which sticky-filled arguments would turn from a read
into a move. A script gets the same variables and keeps the native `PATH` it was
started with.

## run (detached adoption)

So is whatever the last run detached: those processes are still out there, and
the only way to stop them later is to adopt their pids now.

The duration is part of the message because adoption has to finish before the port
is bound: a home with thousands of rows looked exactly like a server that would
not start, and there was nothing to point at. Logged unconditionally,
`adopted = 0` included -- the run that took 75 seconds to bind had 156 stale rows
and nothing to adopt, so the one line that would have said so was the one the
condition suppressed.

## run (inbox nudge)

A message that arrived while a turn was running is normally folded into that turn
at its next step boundary. If the turn ends first, nobody is left to read it --
so a session holding an undelivered message gets a turn started for it. Checked
rather than pushed: the row is durable, so a miss here costs a couple of seconds,
not the message.

## run (roster republish)

The tick that nudges inboxes also carries the roster to clients that are already
connected. The server re-reads `providers.toml` on its own -- every path that
resolves a model asks `providers::roster()`, which re-reads when a file's stamp moved --
but a client holds the table it was greeted with, so the edit would never reach a
running window. The same tick compares the roster's `generation`, which changes only
when a read actually happened, and publishes `Event::Models` -- and `Event::Commands`
beside it, because a catalog is a snapshot of the roster: an argument publishes the
values the roster lets through, so both are stale after one edit.

A broken edit publishes nothing, and that is the point: a roster that stops parsing
serves the last good list under the *old* generation, so clients keep the table they
have until the file parses again. Silence here is "nothing changed", never "the
roster is empty".

## run (the subscription matcher)

One task for every armed pattern, subscribed to the bus. Event publishing does not
consult the registry: every `log_event`, every status change and every streamed chunk
would then pay for a feature most of them never meet. The registry is loaded from the
database before the matcher starts, so a restart resumes what sessions asked to be
told about. See `docs/notes/server-subscriptions.md`.

## run (roster line)

Which roster was read, and whether it was readable at all: "no --model worked" is
nearly always this line.

## run (close_shells)

Every path out of `serve` ends here, so this is the one place that guarantees no
shell outlives the server's decision to stop.

It has to happen before the runtime goes: a live persistent shell is a child that
never exits on its own, and the thread reaping it can never finish while it lives
-- which is exactly the state that made the process sit there after
`server stopped`. Ending the shells is what gives those threads an end, so the
runtime's `Drop` can simply wait for them instead of being given a deadline.

## run (stop_all)

The same rule as the shells, for the other kind of child: a script a command
started is a process too, so it is ended here rather than being left to finish
work nobody is waiting for any more. Its caller is told the server stopped --
not shown a killed process's exit status, which would read as the script's own
failure.

## the_log_is_json_when_a_tool_reads_it

`--log-format json` writes one object per line, and the text format now
colours only a terminal: escapes inside `outcome=Done` made a finished turn
look like one that never ended, and every measurement was a regex away from
being wrong.
