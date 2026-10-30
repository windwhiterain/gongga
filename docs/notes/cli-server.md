# cli/server.rs

## serve

SPEC: `server` is a library and `cli` is the binary that carries it. So "start a
server" is not a program anybody installs -- it is one of this binary's own
client commands, run in a mode where it leaves nothing of the client behind:

```sh
gongga --home ~/.gongga serve --port 9700
```

That is what makes the rest of this module possible: a server to spawn is always
the program you are already running.

It is a *client* command and not a hidden flag, because the command layer is where
"what this program can be told to do" already lives: `serve` shows up in the
catalog, completes, and documents itself like every other command. It is decided
before a tokio runtime exists (`cli::main`), because the server builds the runtime
it wants to be stopped in, and a nested one would panic.

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

Spawns `this program serve --port <p>`, with `--home` named explicitly rather than
left to the config it would read anyway: the child has to serve the home whose
shim the caller just installed.

Its own process group, its own hidden console, and its output appended to
`<home>/server.log` -- all three follow from the same fact: the process that
starts the server is usually gone in milliseconds, so nothing about the server's
life may depend on its starter's terminal.

## install_command

The command an agent's shell types, written by the CLI on every run, because the
CLI is the program that knows where it is. The server only puts `<home>/bin` first
on `PATH` (`server/shell_command.rs`), so the two agree on the home and on nothing
else.

Which of the two shapes it takes is decided by what `cli_binary` found, and the
distinction is the whole of "a user may have only one program":

- the program is already *named* `gongga` -- a script that execs it by absolute
  path (`write_shim`);
- the program only *carries* the command layer -- the same file installed under
  the command's name (`write_named`).

The second case exists because SPEC's window is a transparent wrapper: it contains
the command layer, so on a machine where only the window was installed it is the
only thing that can be a `gongga`. It answers to that name by argv[0]
(`tui-main.md`), which is why installing it under the name is enough -- there is
no flag, no environment variable, and no second program to copy.

## write_shim

Written next to its target and renamed into place. An agent's shell may be reading
the old script at the moment a new `gongga` starts, and a partly written
`#!/bin/sh` is a syntax error in a file that worked a second ago.

It points at the command layer, never at whatever program is running: `gongga-tui`
also carries this crate, and an agent that typed `gongga` must not be handed a
window it cannot see.

## write_named, current, link_or_copy

The file itself, under the command's name: a hard link where the platform has one
(no privilege, no second copy of 25 MB), a symlink on unix first (that one also
follows a rebuild by itself), a copy only when the two live on different volumes.

A link needs refreshing, and that is what `current` is for: a build *replaces* the
program file rather than writing through it, so a link made before a rebuild still
names the old bytes. Length and modification time are compared, and a link that
does not match is replaced -- otherwise a shell would keep running last week's
command layer without a word.

Only one shape may exist in `bin`: `gongga` (the script) and `gongga.exe` are two
answers to one lookup, and which one wins would be decided by the shell's path
resolution. Each writer removes the other's file.

## cli_binary

This process, then its sibling, then `../target/debug` (where `dev.sh` stages a
copy from), then `PATH`, then this process anyway. The staged case is why the
fallback chain exists at all: `.dev/server-<stamp>.exe` is a copy of `gongga.exe`
under another name, so "the binary next to me" is not the binary an agent should
run.

The last resort is the point of `write_named`: whatever is running is the only
command layer there is, and it can be started under the command's name. It sits
after `PATH` so an installed `gongga` still wins over an unnamed copy.

## detach

On Windows the flag is `CREATE_NO_WINDOW`, not `DETACHED_PROCESS`, and that is not
a detail. A process with *no* console at all makes Windows open a **new visible
window** for every console program it starts -- and a server starts plenty: `git`
for the shipped content, `python` for a migration step, `bash` for every shell a
session runs. `DETACHED_PROCESS` was tried first and the screen filled with
flashing windows. `CREATE_NO_WINDOW` gives the server a console that exists and is
never shown, and every child inherits that one instead.

## prepare

The home this process works on, with the command installed in it. One call so that
the window, the one-shot command and the server mode all agree on where state
lives before anything else reads it.

## tests

`only_a_local_address_is_started_here` -- the policy above, both families.
`the_shim_execs_the_command_layer_by_its_absolute_path` -- the script's content,
its line ending, and that rewriting it is safe.
`a_program_with_another_name_is_installed_under_the_command_s` -- the second
shape, and that installing it twice is a no-op.
`a_rebuilt_program_replaces_the_link_that_named_the_old_one` -- the stale link a
build leaves behind, and that it is noticed.
`another_program_is_not_the_command_layer` -- `gongga` in either spelling, and
not the window's name.
`a_home_with_no_key_is_not_answering` -- the key file is progress, not decoration.
`the_key_a_home_carries_in_its_config_is_the_one_to_ask_with` -- the key a home
keeps in `config.toml` is one `answers` can ask with.
