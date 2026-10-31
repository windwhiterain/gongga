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
(`cli-server.md`) -- the command layer is a program of its own now, so the
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
(`server-shell-command.md`); `GONGGA_SESSION` is added where a session's shells are
made, because that name belongs to a session and not to the server. A script gets
the same variables and keeps the native `PATH` it was started with.

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

## run (roster line)

Which roster was read, and whether it was readable at all: "no --llm worked" is
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

## tests::the_database_path_is_a_startup_flag

SPEC: "database path can be specified at server start." Checked by parsing rather
than by reading the source, so that renaming the flag breaks a test instead of
someone's runbook.

## the_log_is_json_when_a_tool_reads_it

`--log-format json` writes one object per line, and the text format now
colours only a terminal: escapes inside `outcome=Done` made a finished turn
look like one that never ended, and every measurement was a regex away from
being wrong.
