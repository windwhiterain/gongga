# `crates/server/src/tools/channel.rs`, `crates/server/src/relay.rs`

## A channel is a program, and a directory

`SPEC.md`'s `exec` starts a program with `bin`, and speaks to it again by `id`. Both
are the same thing: a **channel** — a child process, the two streams it writes, and the bytes of
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

## What an `exec` reads

`out` is a file, so "what did this print" and "what have I not read" are two different
questions: a reader may `tail -f` it, and that is not this server's cursor. The cursor
is the server's own, per session, and taking the window is what advances it — so a
finished channel's notice and an `exec` never hand the same bytes over twice.

`until` is the marker a reply ends at: the window is cut there and the marker is not
returned, because `matched` is the answer to "was it there". A call that names none takes
the name's own (`[channel.<name>]`), and the one `bash` has is `{{sentinel}}` -- the text
this server writes into the command line, `echo __gongga-...__$?`.

That value is sticky per channel (`channels.until_sentinel`): the write that makes one
remembers it, so an `exec` that writes nothing still knows where the last reply ends and
`matched` can answer for it. Generated per write and thrown away, as it was before the
column, a read-only `exec` had no marker at all -- so it could only ever answer at its
`wait`, which is the shape a model then learns to live with: every read costs the whole
ceiling.

`time` is what *this call* cost, and its start is the call's own input: an `exec` that
writes measures from that write, and one that only reads measures from the channel's
last write (the program's start, before any). A channel that has been alive for an
hour answers a fresh command with the milliseconds it took, which is the number a
reader is asking for -- so the row records `wrote_at` beside the cursor.

## `[channel.<name>]`: what a name is

One section per program `exec` knows, keyed by the program's name (a path is read by its
stem), and nothing there is about a tool:

```toml
[channel.bash]
spawn = ["--norc", "--noprofile"]
write = "{{input}}; echo {{sentinel}}$?\n"
until = "{{sentinel}}"
```

`spawn` is what the program starts with when a call gives no words of its own;
`write` and `until` are the framing, and `exit_from_marker` says the marker's tail
carries the status. The table is named for the noun it describes, not for a category
of helper: an earlier draft called it `util`, which said nothing about what it held.

A name the table does not mention is raw: started with the call's own words, framed by
whatever `until` the call states, and by no marker when it states none. `bash` is not raw:
its sentinel is the table's, and the model never carries it.

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
appended. Their shape is part of the remote protocol, for the same reason the frames are:
the peer's relay writes them and this side reads them, so a build that changes the layout
raises `PROTOCOL` instead of pairing with a peer that keeps the old one. A relay started by a peer's runner leaves every job this connection is in, so a
`cancel`, an ssh that drops and a server that restarts all leave the channel running --
which is what `scripts/channel_remote_selftest.sh` proves by killing the server and asking
the same channel again.

## Who reads what

`exec` is a row of `[tool.*]`, like every other tool, and it is `tunneled = false`: a
channel already is what `background` used to mean, and `wait` is the call's own clock
(`0` is no wait, absent is this server's default).

`new_channel`, `input_channel`, `rename_channel`, `ls_channel` and `del_channel` are
commands, not tools: a program started, one line written, one column renamed, a list,
and `kill` in a channel's `in`. They are the command layer's door to the same thing the
`exec` tool is a model's door to, which is what makes a `GONGGA_CLI` channel
and a subscription able to do what a call does. `del_session` ends the channels its
session started, so a name is never left pointing at a dead directory.

The watcher is what turns a wait into a notice, and it speaks only for a channel
somebody is waiting on. `exec` sets the channel's wait flag -- the marker the call
has to see, `""` for the end alone -- and the watcher looks at nothing else. Two things
answer it: the program ending, or the marker appearing in what it printed. The flag is
the claim: whoever clears it is the one that tells. A call clears it when its own answer
is what delivered the event, and the watcher skips a channel a call is inside. SPEC gives
the flag to the call that *ran out* of waiting and to no other: `exec` resets it on the
way in, so `wait: 0`, an `interrupt` and a call its own answer satisfied all leave it
clear. Each of those has already said what it saw, and a flag one of them left behind
would be the same window told twice, a turn after the session stopped listening.

That is also the end of a double notice: one end told twice, once as a result and once
as a notice. Measured on this machine before the change -- four calls in a row returned
`exit: 0` in their own answer and each was announced again a step later. What arbitrates
it was a column update (`ended_at`, "only if not already moved"), and it answered
`Ok(())` whether or not it had moved the row, so the `is_err()` guarding the notice never
fired. The arbiter is the DELETE now (`claim_channel_end`), which says how many rows it
removed, so one end is one speaker.

It is a task on the server's shutdown token, so a `cancel` announces nothing it just
ended: each pass reads `status` and the new bytes of every channel carrying a flag, and
queues an `inject` row marked `notice: "channel"` -- the same door a finished background
job uses. The flag lives in the channel's row (`wait_marker`), so an offer to notice
survives a restart, and it is also what the row means: this session asked to be told,
so the end is news it owes a turn for.

An end nobody waited for is the other row. SPEC still wants it stored ("killing channel /
failed to adopt channel will produce channel end notice"), and the row carries `queued`,
which `owes_a_turn` reads as "the next turn that runs, never a reason to start one".
Without it, closing a server ends every channel a session's last turn left behind, and
the next start wakes that session for each one -- turns nobody asked for, about work
that had already been read.

The notice **is** the answer: the same `Result` an `exec` returns, built by
`Outcome::result` with the same cap, and it takes the window with it -- a notice that
carried the bytes and left the cursor where it was would be the way to hand the same
window over twice. The row is stored as `inject`, so the reader draws it as the harness
talking while the model still reads a user turn.

## An end is the row's end

A channel is the program, so the program's end is what removes its row -- and with the
row goes the name: the next `exec` under it starts a program instead of answering a
corpse with the window it printed before it died. Three places see an end and all three
finish it the same way, by removing the row: a call inside the channel (`answer`, when
the relay's `status` says the program is gone), the watcher (a channel carrying a flag),
and `Channels::reap` -- which `adopt` runs over every row at start, and `exec` runs over
one row when a call asks to start a program under a name that is already taken.

So `ended_at` and `exit` are not a state a channel can be in, and the columns that held
them are gone (`migrate` sweeps the rows an older build left, then drops them). What an
end still owes is the notice: the last thing that channel will say, and the only copy of
its final window. That is why every removal reads first -- `reap` and the watcher take
the bytes, then claim the end with the DELETE, which is the only speaker.

## Still owed: the eighteen go

SPEC's tool surface is `exec` and `wait`; a skill is run by the `run_skill` command and
never by a tool, which is why nothing here carries a skill index. Everything the
channel replaced is still here as a row marked `offered = false` **and** as a helper the
command layer or a subscription still reaches: about 5200 lines across
`tools/bash.rs`, `tools/python.rs`, `tools/interpreter.rs`, `tools/detached.rs`,
`tools/exec*.rs`, `tools/python_remote.rs`, `remote/shell.rs`, `remote/shell_set.rs`.
No name means two things any more: the channel commands are `*_channel`, so a tool
`del_job` (background jobs) and a command `del_channel` are read without a choice to
make. What is left is the tools themselves, which no `[tool.*]` row offers and only a
leftover helper still reaches.

Delete in this order, one slice at a time, each slice building and passing
`channel_selftest.sh`, `channel_remote_selftest.sh` and `tool_selftest.sh`:

1. **Their promises in prose.** The notice texts that name `check_job`/`check_bash`
   as the reader of what a program printed (`bash`'s and `python`'s summaries, the
   finished-job notice) belong to the tools and go with the slice that removes them.
2. **Runners.** `tools/bash.rs`, `tools/exec.rs`, `tools/exec_remote.rs` and the
   `[tool.bash]`/`[tool.remote_bash]`/`[tool.exec]`/`[tool.remote_exec]` rows: a shell
   is a channel now, so a session's `bash` tool is `new bash`.
3. **Interpreters.** `tools/python.rs`, `tools/interpreter.rs`,
   `tools/python_remote.rs`, `remote/shell*.rs` and their rows. Subscriptions already
   run in a channel; the interpreter's REPL semantics are what a `python` channel gives.
4. **Detached processes.** `tools/detached.rs` and the `background` carrier: a channel
   is what outlives a turn, so this is the last to go. It carries `JobRow`, the job
   view and `cancel_lets_go` -- check each before removing it, because `cancel` and a
   `resume` after a stop read them.

## Deleted so far

**`exec`/`remote_exec`**, and the carrier behind them: `tools/exec.rs`,
`tools/exec_remote.rs`, `tunnel::run_own_program` with its `Running` guard,
`tunnel::promote_child`, `words::Own`/`spawn_own`, `Store::job_of_call`,
`ToolContext::endpoint` and the `CANCEL_LETS_GO` flag nothing read any more. Their two
rows went with them, and `scripts/job_selftest.sh` retired too: it tested "a call's own
program becomes a job", which is what a channel is for now. `check_job`/`del_job` and
`tools/detached.rs` are still here for the slice that removes the `background` carrier.

What stays: `read_file`, `write_file`, `replace` (a model has no other door to a file),
`list_paths`, and every command in `commands.rs` that the channel world added.
`Shells::names`, `ToolContext::peek` and the job view are used by live paths today; the
slice that removes their last caller removes them.

## Adopting what a stop kept (`Channels::adopt`, `Channels::stop_every`)

`cancel --keep_alive` leaves every channel detached and says the next start adopts
them. A channel is not a process the server owns -- it is a program a relay keeps,
read and written through its files -- so "adopted" is not a step taken at start: it is
the relay's watcher speaking for a row that is still live. What a start can say is the
other half: which of those rows are not. `adopt` reads each row's status, and one that
has ended is told to its session as a channel end notice -- what it printed, and the exit
it ended with -- and then removed (`Channels::reap`). The notice goes before the turns
that would read it, which is why `run` adopts before it resumes anything.

`stop_every` is the same debt on the way out. A default `cancel` ends every channel, and
with its row gone the relay's watcher can never see it end -- so the kill is the only
voice left, and it has to say so: the end notice carries what the channel had printed
until then. SPEC names this pair as one rule ("killing channel / failed to adopt
channel will produce channel end notice"); the two functions are where it lands.

Both functions take the row's wait flag before they speak, and hand it on as
`ChannelEnd::waited`. That is the whole difference between the two rows: a call was
waiting for this end, so it owes the session a turn, or nobody was, so the notice is
queued. Neither is ever dropped -- the bytes and the exit are the last thing that
channel will say, and a session that reads them a turn later has lost nothing.
