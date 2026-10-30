# cli/server.rs

## serve

SPEC: `server` and `cli` are two programs. So "start a server" is one of this
binary's own client commands, run in a console the server takes over:

```sh
gongga --home ~/.gongga serve --port 9700
```

It is a *client* command and not a hidden flag, because the command layer is where
"what this program can be told to do" already lives: `serve` shows up in the
catalog, completes, and documents itself like every other command. It is decided
before a tokio runtime exists (`cli::main`), because the server builds the runtime
it wants to be stopped in, and a nested one would panic.

Before the server exists -- and therefore before any shell it will start -- it
puts *both* programs in the home's `bin` (`install_commands`, below): the one
moment when the home, this command layer and the server it is about to run are all
known for certain.

`--port` is required and the address is localhost: a server that listened on every
interface would need the operator to think about keys and firewalls, which is a
decision for the config file, not for a one-keystroke start.

## take_over

`serve` is one command, one console and one exit code. On Unix this process is
replaced (`exec`), so the pid, the signals and the code are the server's from the
start. Windows has no `exec`: the child inherits all three stdio handles, this
process only waits, and the server's exit code becomes this one's -- a script that
ran `serve` has to be able to tell why the server stopped.

`--port` is required and the address is localhost: a server that listened on
every interface would need the operator to think about keys and firewalls, which
is a decision for the config file, not for a one-keystroke start.

## ensure

The one place that decides "start one". Called by the window, and only there: a
one-shot command (SPEC: `cli` *can* spawn) is a thing a script or a model runs,
and a script that silently brings up a daemon on someone's port is a surprise, not
a convenience.

Three decisions worth stating, because each could have gone the other way:

**Only a local address is started here.** A dial that fails to `10.1.2.3:9700` is
an outage on another machine; answering it by starting a server for *this* home
would be a server nobody asked for, on a port nobody is listening to.

**A dial decides, not the lock file.** `<home>/server.lock` names a pid, not an
address, and a home can be served on any port the config names -- so "is anything
there" is a question only the wire can answer.

**A refusal counts as an answer.** A server that says no is there, and starting a
second one on the same home would be exactly the split this design refuses.

It installs nothing itself: the pair in `<home>/bin` is placed by whoever starts
the server (`install_commands`), because that is the one place that knows both
programs and the home. A window that starts one, or finds one already answering,
leaves the home's `bin` alone either way.

It does not wait for the server to come up. The window is already retrying with a
backoff, and a second waiter would only duplicate that loop with a worse error
message; what `ensure` has to get right is *starting* the thing, not watching it.

## starts_here

`is_loopback()` rather than a comparison against `127.0.0.1`: `[::1]` is the same
machine, and a config that binds IPv6 should not read as a remote server.

## answers

The key is the first evidence of progress: a server can only be greeted by
someone who has it, so a home that carries none has no server to talk to. It is
read the way `dial` reads it -- `server.key` first, because that is where a
minted key is written, then a `[server] key` in `config.toml`. One reader for both
questions: reading only the file made `answers` say no to a server that was
running with a configured key, so every window that opened started a second
server to be refused by the lock.

Once there is a key the question is a real handshake -- and a `DialFailure::Fatal`
(wrong key) is still a server, while `DialFailure::NoListener` is exactly the case
this function exists to detect.

## start

Spawns `gongga-server --home <home> --bind <addr>`, with `--home` named explicitly
rather than left to the config it would read anyway: the child has to serve the
home whose command the caller just placed.

Its own process group, its own hidden console, and its output appended to
`<home>/server.log` -- all three follow from the same fact: the process that
starts the server is usually gone in milliseconds, so nothing about the server's
life may depend on its starter's terminal.

## server_program

Where the program that carries the server is, and there are exactly three answers:

1. `GONGGA_SERVER_BIN`, when something named one -- a test, a launcher, an
   operator holding two builds;
2. the sibling of this program: a build tree and an install both keep the pair
   together, so the `gongga-server` beside `gongga` is the same build;
3. `<home>/bin/gongga-server`, the copy an earlier start of this home installed.

Never a `PATH` search. A machine can hold several builds, and the one that answers
`PATH` is not necessarily the one this command layer was built against -- that is
the skew `this_build` exists to prevent, and a search would reintroduce it from the
other side. If none of the three is there, the error names all three.

## install_commands

The two commands a home's shells and scripts see: `gongga` is *this* program,
`gongga-server` is the one about to serve, and both are placed before the server
exists -- the one moment when the home and both programs are known for certain.

The server goes in first. A `gongga` refreshed without a `gongga-server` beside it
would be a command whose server cannot be found, and the failure would show up one
level down, in a `serve` nobody is watching.

Both, or the pair drifts: an operator who updated `gongga` by hand would leave an
old server behind it, and the shells of that home would drive a build the server is
not.

## place

`<home>/bin/<name>.exe` on Windows, `<home>/bin/<name>` elsewhere, for each of the
two programs. A link, not a wrapper script, and that is what makes one name
enough:

- bash resolves `gongga` to `gongga.exe` by itself,
- a native process starting `gongga` finds the same file,
- so `subprocess.run(["gongga", ...])` in a script needs no path, no variable and
  no knowledge of which platform it is on.

`a_home_gets_both_programs_from_one_install` is the pair; `the_server_is_looked_for_in_three_named_places`
is the lookup, including that a missing server reports every place it looked.

The link is a hard link on Windows and a symlink elsewhere. A symlink follows the
path through a rebuild by itself; a hard link cannot, so each start compares size
and modification time with the running program and replaces the name when they
differ -- a last week's build answering to `gongga` while this week's serves the
home is exactly the skew `current_exe` exists to prevent.

It is placed beside its target and renamed in, because the command may be running
right now -- and where Windows refuses to replace a running executable, the old
one stays until the next start.

## link_or_copy

A hard link cannot cross a volume, and a home may sit on another drive than the
program serving it. The copy is the fallback that keeps the name working, at the
price any copy pays: it is the bytes of one build, and the next rebuild makes them
yesterday's.

## detach

On Windows the flag is `CREATE_NO_WINDOW`, not `DETACHED_PROCESS`, and that is not
a detail. A process with *no* console at all makes Windows open a **new visible
window** for every console program it starts -- and a server starts plenty: `git`
for the shipped content, `python` for a migration step, `bash` for every shell a
session runs. `DETACHED_PROCESS` was tried first and the screen filled with
flashing windows. `CREATE_NO_WINDOW` gives the server a console that exists and is
never shown, and every child inherits that one instead.

## tests

`only_a_local_address_is_started_here` -- the policy above, both families.
`the_command_is_this_program_under_the_shell_s_name` -- the name, the bytes, and
that there is only one copy of them.
`a_stale_command_is_replaced_by_the_one_running` -- the rebuild case.
`a_current_command_is_left_in_place` -- and the home is not churn on every start.
`a_link_that_fails_falls_back_to_a_copy` -- the other volume.
`the_other_spelling_is_removed` -- one directory, one answer to the lookup.
`a_home_gets_both_programs_from_one_install` -- the pair, in one step.
`the_server_is_looked_for_in_three_named_places` -- named, sibling, home; and the
error reports every place it looked.
`a_home_with_no_key_is_not_answering` -- the key file is progress, not decoration.
`the_key_a_home_carries_in_its_config_is_the_one_to_ask_with` -- the key a home
keeps in `config.toml` is one `answers` can ask with.
