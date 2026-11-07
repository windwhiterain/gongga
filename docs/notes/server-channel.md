# `crates/server/src/tools/channel.rs`, `crates/server/src/relay.rs`

## A channel is a program, and a directory

`SPEC.md`'s `new` starts a program and `check` speaks to it. Both are the same
thing: a **channel** — a child process, the two streams it writes, and the bytes of
them nobody has read yet.

Its path is a name in this server's namespace, not a place on disk (`SPEC`'s
`id: path`, and the decision that the files are internal). On disk one channel is a
directory under `<home>/channels/ch-<opaque>/`:

| file | who writes | what it is |
| --- | --- | --- |
| `cmd` | the server, once | the words the program was started with |
| `out` | the relay | its stdout and stderr, append-only |
| `status` | the relay | pid, start, exit, end; rewritten whole by a rename |
| `in` | the server | commands: `input <base64>` or `kill` |

The name is not the path because a live program's files cannot be moved: a Windows
move of a directory whose file is open is refused. Measured on this machine with a
junction standing in for the name — the link could be created and removed while the
target's file was held open, and the target itself could not be moved. Since the
name is only ever a namespace key, nothing has to move.

## The relay is what makes a channel outlive the server

A pipe belongs to the process tree that made it: when the server dies, its end of the
child's stdin is gone and no later process can re-open it. So the child is not the
server's — it is a **relay**'s (`gongga-server --relay <dir>`), started detached, and
the relay is what the server talks to through two files. The server may die and come
back: `out` keeps growing, `status` says what became of the program, and `in` is still
appendable.

That is why `--relay` is answered before clap and before the watchdog: a supervised
relay would be started again the moment its program ends.

The `.run` of a relay is a loop of two questions — are there new commands in `in`, and
has the child ended — every 50 ms. The poll is the floor on a write's round trip, and
a `kill` also waits for one.

## The relay pumps the streams; it does not hand the child a file

Both streams are pipes the relay reads, and it appends what it reads to `out`. The
first design handed the child an inherited file handle for stdout and stderr, and
Git's bash on this machine — the program an agent runs most — then said nothing at
all: its status came back (a command that ended with `exit 3` answered 3), and `out`
stayed at zero bytes. `cmd.exe` writing to the same handle worked, and the same bash
writing to a file opened by another parent worked, so the handle was sound and the
program was not. Pumping through a pipe is also what interleaving wants: two streams
of one program in one file, in the order they arrived.

## What a `check` reads

`out` is a file, so "what did this print" and "what have I not read" are two different
questions: a reader may `tail -f` it, and that is not this server's cursor. The cursor
is the server's own, per session, and taking the window is what advances it — so a
finished channel's notice and a `check` never hand the same bytes over twice.

`until` is the caller's marker: the window is cut at it and the marker is not
returned, because `matched` is the answer to "was it there".

## `[channel.<name>]`: what a name is

One section per name `new` knows, and nothing there is about a tool:

```toml
[channel.bash]
spawn = ["--norc", "--noprofile"]
write = "{{input}}\n"

[channel.GONGGA_CLI]
kind = "cmd"
```

`kind` is `program` unless it says `cmd`; `spawn` is what the program starts with when
a call gives no words of its own; `write` and `until` are the framing, and
`exit_from_marker` says the marker's tail carries the status. The table is named for
the noun it describes, not for a category of helper: an earlier draft called it `util`,
which said nothing about what it held, and `[channel.*]` is also what makes a `cmd`
entry honest -- a `cmd` is a channel this server answers itself, so it belongs beside
the ones it starts.

A name the table does not mention is raw: started with the call's own words, framed by
the call's own `until`. That is why `bash` has no `until`: its sentinel is the caller's
business and the model never carries it.

## A channel on a peer

`new --remote <name>` starts the program on that machine, and the channel means there what
it means here: a directory, a relay that outlives this server, and two files this process
talks to. Three things differ, and each has one reason.

**The peer chooses the directory.** Only the peer knows which home it has -- `GONGGA_HOME`
there, else `~/.gongga` -- so `Op::Relay` carries no path and answers one, the way `open`
answers the directory it accepted. The client stores that spelling and hands it back for
every read and append; it never builds a peer path of its own. The peer writes `cmd` and an
empty `in` before it starts the relay, so no write races the relay's first look, and the
words travel inside the relay request rather than as a file this side names.

**The program runs with the peer's environment.** This machine's `PATH`, `GONGGA_HOME` and
`GONGGA_CLI` name paths the peer does not have, so a channel there is started with no
environment of ours -- the same rule a remote shell already follows.

**The connection is the server's, one per peer name.** A channel is read on every look, so
the runner is kept; a connection that has ended is not reused, because a peer that was
restarted must not answer with the silence of the run before it. It is not a session's
connection: the channel outlives the session's shells either way.

Everything else is the same three files: `out` from the row's cursor, `status` whole, `in`
appended. A relay started by a peer's runner leaves every job this connection is in, so a
`cancel`, an ssh that drops and a server that restarts all leave the channel running --
which is what `scripts/channel_remote_selftest.sh` proves by killing the server and asking
the same channel again.

## Who reads what

`new` and `check` are rows of `[tool.*]`, like every other tool, and both are
`tunneled = false`: a channel already is what `background` used to mean, and `wait` is
the call's own clock (`0` is no wait, absent is this server's default).

`ls_job`, `del_job` and `rename_job` are commands, not tools: `ls` of a session's
channels, `kill` in a channel's `in`, and one column renamed. `del_session` ends the
channels its session started, so a name is never left pointing at a dead directory.

The watcher is what turns a program's end into a notice. It is a task on the server's
shutdown token, so a `cancel` announces nothing it just ended; each pass reads `status`
for every channel whose row says it has not ended, records the end, and queues an
`inject` row marked `notice: "channel"` -- the same door a finished background job
uses, and marked so it cannot wake an idle session.

The notice does **not** take the window. It carries a preview for the reader and says
`check` has the rest, because a notice that consumed the output would be the one way
to lose it: the cursor moves when someone asks, not when the program ends.

## Still owed

- A `cmd` name (`GONGGA_CLI`) reaches the command layer through `commands::layer()`, a
  trait object installed once, holding a weak dispatch: the dispatch owns the agent
  whose tools call it, so a strong one would keep both alive. The caller's session fills
  the first argument the target declares as a session, and only when the call named
  none. A `cancel` a tool runs is a client's: the `self-control` skill is what says a
  tool may do what a person may.
- The old eighteen tools are rows marked `offered = false`: the model is told about
  `new`, `check` and `skill` only, and the rest answer to their names for the command
  layer (`gongga-cli bash …`) and for a subscription action. They are deleted -- not
  merely unoffered -- when a name routes to the command layer (`GONGGA_CLI`) and those
  actions move onto channels of their own. Deleting them first was tried and reverted:
  each removal takes the helpers a *live* path also uses (`cancel_lets_go`,
  `ToolContext::peek`, `Shells::names`, the job view), so the two go together.
- An automatic id carries the program's name (`main/bash/2`). It does not have to: the
  counter alone would do, and the name is the one thing a rename makes wrong.
