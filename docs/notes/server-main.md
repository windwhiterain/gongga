# server/main.rs

## Args::db

SQLite database path. Overrides `[server] db`.

Missing directories are created, and `:memory:` gives a throwaway database --
which is what makes a clean end-to-end run possible without touching the
operator's real one.

## Args::home

The home directory: the database, config, logs, detached-job records and the
instance lock all live under it. Overrides `[server] home`, which overrides
`$GONGGA_HOME`.

## cli_path

The server and the command line ship together, so the usual answer is the sibling
of this executable -- `gongga`, the command layer's own binary, not the window.
That is not the only layout in use, though: `dev.sh` copies the server into
`.dev/` *alone*, so the sibling is missing there and the agent ends up with a
`GONGGA_CLI` that is not on `PATH` -- the one thing it needs in order to drive (or
replace) the server it is running on.

So: sibling, then the `target/debug` a staged copy sits under, then `PATH`, and
only as a last resort the bare name -- announced, because a bare name that is not
on `PATH` is a dead end the agent cannot diagnose. Whatever comes out is also what
`shell_command::install` puts in the shell's `gongga` command, so the two cannot
disagree about which binary is being driven.

## main

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
placed yet. `config_from` exists so the flags win in both reads -- `--db` on the
first keeps the lock and the home agreeing, and on the second keeps being this
run's database rather than moving every later run to it.

## run (instance lock)

Before touching the database: two servers on one home would each resume the same
pending turn and drive the same session twice.

## run (shell env)

Detached jobs live next to the database: they outlive this process, so their logs
have to outlive it too.

How the model drives itself, which is what this vec is for. `bash` runs inside
this process's children, so the server address, the CLI that speaks to it and the
session it belongs to have to be handed down rather than discovered: without them
an agent that wants to cancel or restart has no better option than killing its own
server from outside. `GONGGA_HOME` and `GONGGA_SERVER` are exactly the two
fallbacks the CLI reads, so a command needs no flags of its own. `PATH` is what
turns `$GONGGA_CLI` into `gongga` -- see `server-shell-command.md` -- and
`GONGGA_SESSION` is added where a session's shells are made, because that name
belongs to a session and not to the server. A script gets the three variables and
keeps the native `PATH` it was started with.

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

## tests::the_cli_path_is_something_that_can_be_run

The CLI is what lets the model cancel, restart or replace this server, so the
path it is handed has to be runnable -- a bare name is only correct if that name
is actually on `PATH`.

## tests::the_database_path_is_a_startup_flag

SPEC: "database path can be specified at server start." Checked by parsing rather
than by reading the source, so that renaming the flag breaks a test instead of
someone's runbook.
