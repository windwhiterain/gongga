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

## Still owed

- `new` and `check` as rows of the config, over `Channels`.
- `write`/`until`/`exit_from_marker` read where a call names none: the framing a
  `[channel.<name>]` row declares, and the sentinel that carries `bash`'s status.
- The notice a finished channel owes its session, and the offset it reads from.
- `ls_job`, `del_job`, `rename_job`: `ls` of the session's channels, `kill` in `in`,
  and a renamed namespace key.
