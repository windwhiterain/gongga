# `crates/server/src/remote/`

A session's shell may run on another machine. What crosses the wire is a **runner**:
one stdio program on the peer that owns that machine's shells and answers requests
by id. See `.agents/notes/2026-11-01-remote-is-a-shell-endpoint.md` for the decision.

## `remote::protocol`

Protocol 2 added `args` to `Op::Open`, so a program other than a shell can be started
there -- `remote_python` is its one caller. The bump is deliberate: a runner that
dropped `args` would start `python` with `--norc`, a wrong answer rather than a
missing one, and an older runner is refused by version before anything is sent.

One JSON object per line, in both directions, tagged so a frame says what it is:

- a **request** has an `id` and an `op`;
- a **reply** repeats the `id` and carries either `ok` or `err`;
- an **event** has an `event` field and no id: the greeting, a shell's output, a
  shell's end.

`Op::Open / Write / Signal / Close` are the shell's; `Op::Stat / Read / List` are
the file side. Files travel base64 because a line has no binary.

`Event::Out` carries `seq` per shell and per stream, so a client can tell a gap
from a quiet job; `Event::End` carries the exit status the peer itself observed.

## `remote::runner` (`gongga-runner`)

Launched by `ssh <peer> gongga runner`: it listens on no port, holds no session,
and knows nothing about models. Its runtime is the peer's own.

- every request runs on its own task, so a signal, a stat or a file read is
  answered while a shell is inside a long command;
- a shell is started in its own process group where the platform has one;
- the supervisor task owns the shell's `Child` and is the only thing that ends it;

`end_group` answers `confirmed` only when the group was *observed* empty — on
POSIX after TERM, a grace, KILL, and a second look; on a Git Bash peer through the
job object the shell was put in. Silence, a timeout, or an endpoint that could not
look is `unknown`, which is not the same as stopped.

### Why a Git Bash peer uses a job object, not a tree walk

`taskkill /T` misses the children Git Bash re-parents: `bash -c 'sleep 30'` runs
`sleep` in a process whose Windows parent has already gone, so the walk that ends
the shell finds the bash tree and leaves `sleep` running. It was observed doing
exactly that. The shell is created suspended, joins its nested job
(`JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`), and only then has its initial thread resumed.
Membership follows every descendant including re-parented ones, even when sshd
already owns the runner's parent job. A failed join or resume ends the child rather
than starting a shell this runner cannot own. `terminate`
plus `ActiveProcesses == 0` is the proof; a member that left the job would not be
counted, and nothing in this stack leaves it.

### Invariant: the leader is not reaped before its group is signalled

A reaped pid can be handed to another process, and a signal addressed to "the
group with that id" would then reach a group that was never ours. The supervisor
therefore signals first and reaps after, and it holds the `Child` for exactly that
long. This is also why close confirmation is never a bare `kill -0`.

### Invariant: no work is accepted for a shell that is closing

`close` sets `closing` and then takes the stdin lock before signalling, and `write`
takes the lock before it checks the flag. A write the endpoint accepted therefore
finished before the signal; a write that arrives afterwards is refused rather than
sent into a half-dead shell. The shell's entry stays in the map until its group is
gone, so a late write or signal is answered "no such shell", never aimed elsewhere.

When the client's channel closes, the runner ends every shell's group before its
process exits; anything it could not confirm is logged, since the client that would
be told has gone.

### Invariant: `End` follows every byte

Each pipe's reader signals completion only after its last frame has been written,
and the supervisor waits for both signals before it emits `End` — for a natural
exit and for a close alike. A client stops reading at `End`, so a byte emitted
after it is a byte lost. A daemonized escapee can hold a pipe open forever, so the
wait is bounded; the shell is then reported as having lost its tail, never as
having printed nothing more.

### Flow control: pausing the pump, not dropping the frame

The client's reader must never block on one shell: an answer, a signal and another
shell's output all travel the same connection. So each shell on the client has a
small window (`WINDOW` frames). Filling it sends `Op::Pause` for that shell and the
peer's pump waits instead of reading on; draining it sends `Op::Resume`. Nothing is
dropped, nothing grows without bound, and every other request keeps its answer.

**The pause is a level, not a wake.** A shell has two pumps -- stdout and stderr --
and both must stop and both must start again. Held as a flag plus a `Notify` whose
resume was `notify_one`, exactly one of the two woke: the other slept through every
later resume, and since a sleeping pump emits nothing, the shell answered no command
again -- not slowly, but never. Measured over ssh: 21 good rounds, then every round
after it returned `still_running` with zero bytes until the process was killed, and
the peer's own log ended in `WAIT Stdout` and `WAIT Stderr` with no wake between them
for 388 seconds. The state is a `watch` channel now: each pump keeps its own receiver
and reads the value, so a resume that arrives before a pump looks is a value it sees,
not a permit it missed.

**One owner sends the flow ops.** On the client, both the filling window and the
draining reader used to decide, each spawning its own request. Two tasks deciding
means two ops whose arrival order is unconstrained: a `Resume` overtaking its own
`Pause` leaves the peer paused, and nothing later resumes it because the client
believes it already has. One task per shell now reconciles: it compares what the peer
was told with what the queue holds *now* and sends only a difference, so what the
peer holds is a level the client chose. Paused until the queue is empty, paused again
only at the window -- hysteresis, so one frame in and one out is not a round trip.

When a shell ends, its pumps read an unpaused level, since the last bytes are exactly
what `End` is waiting for.

On the peer, a write that fails cancels the whole run: shells are stopped rather
than left running where their output has nowhere to go, and requests that arrive
are answered with the reason.

Observed platform difference, recorded rather than claimed away: writing to a pipe
whose read end is closed fails at once on unix, and was observed to *block* under
Windows. The closed-reader test is therefore unix-only; the Windows case is bounded
backpressure, not a lost frame.

## The channel operations

`Relay` is the one operation that starts something: it is what makes a channel on a peer
mean what one here means, and it is described in `docs/notes/server-channel.md`. In short:
it carries the channel's `cmd` words rather than a directory, and answers the directory the
peer chose and the pid it runs -- the peer is the only side that knows which home it has.
`Append` and `Read` are what this process then speaks with, and both are ordinary file
operations: append to `in`, read `out` from the row's cursor, read `status` whole.

The protocol version was raised for each of them in turn -- `Append` is 3, `Relay` 4 and
`Relay`'s words 5 -- so a client refuses a peer it cannot speak to instead of discovering it
mid-command.

## Getting the runner onto a peer: this build places it

A runner speaks one protocol and a server speaks one, so what a pair has to agree on is
that protocol -- not the commit either was built from. Naming a path per build made it an
operator's problem: a path says where a program is, never what it says, and two builds of
one protocol would each have been copied to a peer that could have served both.

So the payload is named by what it promises: `<peer home>/.gongga/payload/<protocol>/`
holds this protocol's `gongga-runner` and the `gongga-server` a channel's relay needs
beside it. Three rules follow, and each is a consequence of the first.

1. **A `runner` key in the remote's table is a preference.** It is greeted first, and kept
   only if it greets this protocol; anything else about it is reported and run past.
2. **A peer that already holds the directory is answered without a transfer**, whichever
   build put it there. Identity is what the greeting says: a runner that answers this
   protocol *is* this protocol's runner, and the pair stands as it is. That is what makes
   a rebuild harmless -- the earlier failure was a size of a file on disk deciding whether
   two machines could talk, a fact the other end can neither see nor answer.
3. **The directory is written once and never replaced.** A program the peer may be running
   cannot be overwritten while it runs, so asking for that is the failure this rule
   removes: each copy goes to a staging name of its own and is renamed into place whole,
   which makes "present" mean "whole", and two clients placing at the same moment do not
   share a staging file. A pair that is present and does not answer is reported, never
   replaced -- replacing it is a decision for a human.

A copy goes through `scp`, not through the shell: `ssh … bash -lc 'cat > file'` was
measured to deliver 947256 bytes of a 1048576-byte file, and an executable that arrived
short is not a program. A transfer can still fail for its own reasons (the network, a full
disk, a permission), and that is why it is off the path that decides whether two ends can
talk: it can only make the *first* placement fail, never make a peer that already answers
stop answering.

The first use on a peer that holds nothing for this protocol costs one copy of this build
(tens of megabytes over ssh); every use after it, from any client, is a greeting.
`remotes.toml` needs no per-build edit. Payloads of retired builds are left where they are:
nothing reads a directory whose number this build does not speak.

### The same architecture, not just the same kernel

`ssh` starts the peer's own program, so the runner must be a binary the peer can
execute. Nothing here cross-compiles for you: a macOS server talking to a Linux
peer needs the Linux distribution on the peer, which means building the bundle on
(or for) Linux. `runner` is configurable precisely so a peer that already has a
copy somewhere else can be named instead of re-installed -- and a mismatched pair
is refused at the greeting rather than half-working, because the protocol version
is checked before any shell is opened.

What is *not* needed for a Windows server and a Windows peer to agree: the
session's shells are the peer's own `bash` (Git Bash there), its own paths, its
own pid space. The server never signals one of those pids.

## `remote::transport`

The client half: one child process (a local runner now, `ssh` later), a writer
task, a reader task, a stderr task, and a reaper.

- `Runner::spawn` waits for the greeting and refuses a peer whose protocol version
  differs, so an incompatibility is not discovered mid-command;
- `Runner::request` pairs a request with its answer by id, through a `oneshot`;
- `Runner::open` gives every shell its own channel, so one shell's output cannot
  be read by another and nothing is dropped for being unread;
- a closed reader wakes every waiter with the reason, so a dead transport is an
  error and not a hang;
- `ShellHandle::close` answers from the endpoint's report only;
- `Runner::open_with` is `open` with the program's own arguments, which is how the
  endpoint starts an interpreter instead of a shell;
- `ShellControl` carries the shell's queue as well as its runner, so a watcher can
  read a peer file (`read`) and hear the shell end (`ended`) without taking the
  handle's lock — which is what a background job's tailer needs while its shell is
  busy running something else.

### Invariant: two clocks, and neither is the other's

There are exactly two, and they answer different questions:

| clock | starts | ends with | on expiry |
| --- | --- | --- | --- |
| the call's own `timeout_ms` | the tool call, on this machine | the call's command running | a background job: the command keeps running and its output is posted later |
| `[remote] answer_timeout_ms` | a request (or a greeting) sent to a peer | the peer's silence | an error naming the operation -- never a background |

Neither may move the other. The background clock is measured on this machine alone:
a peer that answers nothing does not delay it, and the turn stops waiting on
schedule. The answer clock is the only thing a `request` may wait on, so no round
trip is unbounded -- `Open`, `Write`, `Stat`, `Read`, `List`, `Space`, `Signal` and
`Close` alike, and `ShellHandle::write` with them.

Measured, before this invariant: a `remote_bash` carrying `timeout_ms: 60000`
was still `running` two hours later, because its deadline covered `pump` and
nothing before it -- `connect` and `Op::Write` each waited for an answer that
never came, no clock was on either, and no promotion is possible for a call that
never reached its own deadline.

`DISCONNECT_GRACE` is the one place this file waits on purpose: when the last
owner drops, the child is *not* killed at once. The runner learns of the end from
EOF on its own stdin and ends its shells itself; killing it first would abandon
them, which is the silence a disconnect must not produce.

## `remote::endpoint`

Three kinds of endpoint, and only one of them can come from a config file:

- `Local { program }` — a shell here, exactly as before;
- `Remote { transport, runner, answer }` — `ssh -o BatchMode=yes [-p port] [-i key] host --
  runner`. Nothing weakens host key checking: `BatchMode` only stops a password
  prompt from hanging a turn, and a host that is not in `known_hosts` is refused by
  `ssh` itself, which is the answer a caller wants.
- `Direct { runner }` — a runner process on this machine, which is how a peer is
  exercised without ssh. **Test-only**: a `[remotes.<name>]` table has `host`, `port`,
  `identity` and `runner`, and parsing can only produce `Endpoint::Remote`, so no
  TOML file reaches `Direct`; `local`, `none` and hostless entries are refused at load
  (`docs/notes/server-remotes.md`).

`answer` is the link's clock, carried here because it belongs to the reach and not
to a tool: `Endpoint::answer_timeout` is what every runner spawn and every request on
this endpoint is bounded by. `Local` and `Direct` answer with
`DEFAULT_ANSWER_TIMEOUT`, since a runner this machine started itself can be silent
too.

A remote directory is the endpoint's to accept: `open` canonicalises it on the peer
and an unknown one is a hard error, never a substitution of this machine's.

## `remote::shell` and `remote::shell_set`

`ShellSet` is the session's shells, wherever they run; `Shell` is either the local
`BashShell` or a `RemoteShell` speaking the same sentinel protocol, so every tool is
written once. A `RemoteShell` is built **unconnected**: it is created while the
session manager's live lock is held, and no connection may be made there.

Two rules a reader should not have to re-derive:

- **a session's target moves only by ending every shell.** It was written once at
  creation and never changed, for a reason that has not gone away: a lazily created
  remote shell has never connected and has no pid to compare, retired shells outlive
  the names they were filed under, and a shell created on the peer cannot be carried
  off it. The maintainer asked for a move anyway (2026-11-02), so the rule is now that
  a move *is* the teardown: `SessionManager::relocate` ends every shell and its
  retired ones, reports what could not be proved, and only then swaps in a fresh
  `ShellSet` on the new endpoint. `ensure_on` still refuses a different target, so
  creation and moving remain two paths that cannot be confused.
- **a move asked for during a turn waits for that turn to end.** The turn holds the
  session's gate, read the shell set at its start and keeps it through every tool
  call, so a move landing under it would end shells a call is using. It is queued
  rather than refused, and the maintainer gave the reason: **the one caller that wants
  to move a session is the agent working inside it, which is always mid-turn**. A
  refusal would make the feature unreachable by the only party that reaches for it.
  The command therefore answers "will move when its turn ends" and returns at once,
  so nothing waits minutes on a lock; `spawn_turn` applies the queue after the turn,
  holding the gate, and says so in the log.
- **the move and the row land together.** Both halves of the placement are one
  statement, because a row naming the new machine with the old machine's directory
  would hand the next start a path that cannot exist there. A move with no `--cwd`
  keeps no directory at all, which is what makes the endpoint choose.
- **a remote session's directory is the endpoint's.** `--cwd` is passed to the
  runner and canonicalised there; a path that does not exist on the peer is an
  error. With no `--cwd` the row keeps no directory at all and the endpoint chooses
  its own, after which the reported path is adopted as the shell's directory and
  written into `sessions.cwd`, so the next start begins where the endpoint is.
- **a stated directory is checked before the move.** `set_session` asks the
  destination -- `is_dir` here, `Op::Stat` through the remote's `Reader` on a peer --
  and refuses the command when the answer is no. Both machines are asked the same
  question, so a local `--cwd` no longer falls back to the server's directory and
  reports `ok`; a session whose row already names a removed directory still starts,
  because the check is on what a caller states, not on what a row records.
- **a path kept or handed to a shell is the endpoint's spelling.** The peer answers
  it -- `Op::Stat` canonicalises, drops the verbatim `\\?\` prefix and uses `/`
  throughout -- and that is what is stored in the row, what `open` is given back, and
  what a listing command puts in a shell word. Git Bash's `pwd` answers `/c/x` for a
  directory opened as `C:/x`, and a drive-less path is not one the peer can open
  again: adopted as said, it made one working session fail every later shell with
  `no such directory on the endpoint`. `pwd -W` asks for the native spelling first
  and plain `pwd` remains the fallback for a POSIX peer.
- **a listing shell the peer has lost is replaced.** The shell `list_paths` keeps is
  reused across completions, and a peer that has dropped its end answers every later
  run with `no shell 1 is open`; one failed run forgets the shell and dials a fresh
  one, so a bad minute costs one listing instead of every completion after it.

### Endpoint shells are ended without their lock

A shell's own `Arc<Mutex<..>>` is held for as long as its command runs, so a
teardown that needed it could wait forever — and did, before this. Each endpoint
shell also has a **door**: one shared state saying whether the shell is unopened,
opening, connected (with a clonable `ShellControl` = runner + shell id), retired, or
closed with the outcome.

- `del_bash` and session teardown ask the door, never the lock, so a shell busy with
  a `sleep 30` is ended while the reader still holds it. Deleting a name ends its
  active shell and every retired shell of that name, not just its replacement;
- the close is **single-flight**: the first caller to find `Connected` closes, and
  every later caller — a second `del_bash`, the command owner noticing
  `out_of_sync`, teardown — waits for and receives the *same* report. None of them
  ever invents `confirmed`;
- retirement and publication are one state, so a connection that completes after its
  shell was retired is handed back to its opener and closed there, and a command
  arriving for an already retired shell is refused instead of opening one on the
  peer that nothing would end;
- a shell that was never opened reports `confirmed` with nothing to end, and a shell
  that is still opening when it is retired reports what its opener found — an
  unobserved connection is never called stopped;
- a `Discarded` value that is dropped without being ended still ends its shell in a
  spawned task, which is the net under an owner that forgot. It can only log: the
  caller that asked already has its answer.
- the peer's pid is **not** kept as a local pid. Only a local shell's pid is
  recorded, and only a local shell is ever given to `kill_tree`; a peer's pid names
  an unrelated process here.

## A session with a remote has shells on two machines

What a remote session may not do is now a short list, because the answer to most of
it is the other tool rather than a refusal. `bash` runs on this machine and
`remote_bash` on the session's remote; `check_bash`, `del_bash` and `list_bash` take
a name and find it on whichever machine holds it, and say which one that was.

The session's directory belongs to one machine: the remote's when it names one, and
this machine's otherwise. That single fact answers "whose file is this" for the
prompt's index, for `@file(...)`, and for `@image(...)` -- nothing infers a machine from how
a path is spelled. A reference in a tool's output is read on the machine that ran the
tool; a reference in a user's message, on the machine the session stands on.
See `.agents/notes/2026-11-02-a-session-has-two-machines.md`.

Still refused, each with a message and never a local fallback:

- `bash` or `remote_bash` with `background: true` on the remote — the job's scratch
  file and completion watcher are this machine's;
- `detached: true` anywhere but this machine — the process would be started here and
  outlive everything asked of the peer;
- a capped output on a remote is not spilled to a file: the path would name this
  machine, so the model is told the tail was dropped instead;
- `remote_bash` with a `target` that is not this session's peer.

## The file tools reach the peer

`read_file`, `write_file` and `replace` run the Python driver on this machine by
default, and on the session's remote when the call's `remote` names it -- the same
one machine `remote_bash`'s `target` accepts, and for the same reason: a session's
directory is on one peer, so a second name is refused rather than quietly becoming
a file here. The remote driver is `remote_python`'s, started by name on the peer,
so the three verbs need no protocol of their own and a peer without Python refuses
the call rather than writing on this machine.

The machine a call ran on is the machine its output's references are read from, so
`Tool::machine` takes the call's arguments: a file read on the peer is expanded
there, not here.

## A background job of a peer's shell

`remote_bash` with `background` starts a job in the session's own shell on the
peer, exactly as `bash` does here: one builder writes the wrapper
(`background_script`), so the two machines cannot drift. The wrapper runs the
command in a subshell, sends its output to a file, and writes its status after a
sentinel the job cannot guess. `background_done_marker` makes that marker.

Three things differ, and each is a question only the peer can answer:

- **where the file is.** The endpoint's temporary directory is asked once per shell
  with `( command cd "${TMPDIR:-/tmp}" && { command pwd -W 2>/dev/null || command pwd; } )`
  and kept: a subshell, because asking must not move the shell's own directory, and
  the native spelling, because this side stores the answer and hands it back to a
  file read. Measured: `C:/Users/Admin/AppData/Local/Temp`, 646ms cold, once.
- **how it is read.** `Op::Read` through the same runner, from the offset already
  consumed. Measured: **15-16ms** per poll on a small file, so a 150ms poll costs
  about a tenth of one connection. A poll is a request, not a byte stream, which is
  why the same read of a 4.7MB binary cost 57ms: the bytes were the price, not the
  asking.
- **when it has ended.** Two events, never a clock: the wrapper's sentinel appears in
  the file, or the shell's channel ends — the job is a child of that shell, so a dead
  shell is a dead job. `ShellControl::ended` waits on the shell's own queue, which is
  how a watcher hears that without taking the shell's lock. There is no pid file here
  and no `kill -0`: this machine cannot ask a peer whether a pid is alive, and it does
  not need to. Measured on a killed shell: the notice arrived in 295ms with no status,
  which is the honest answer.

The file is removed by the shell that made it (`command rm -f … &`, no output on the
shell's pipes). A shell that dies with a job running leaves that file behind — one
small file in the peer's temp directory, which is the price of not having a delete of
our own there.

What is *not* different: the notice. It is `BackgroundEnd`, so the same
`background_notifier` queues the same inbox row, and `check_bash` reads the same
unread buffer — both were already machine-agnostic, and needed no change.

## `target`: one machine, written out loud

SPEC spells the tool `remote_bash{command, name, target = session's remote}`, and
`target` was missing from the schema. Not declaring it was not harmless: a key no
schema declares is passed through and **ignored** -- that is this codebase's decided
rule for a stray argument -- so a model writing `target: "other-peer"` ran its command
on this session's peer and was told nothing. Declaring it is what makes the value
reachable, and `BashTool::run` refuses any name but the session's own:

    `remote_bash` ran nothing: this session's directory is on remote "a", and its
     shells are there -- "b" is another machine, which this session has no directory
     on. A session works on one machine: `set_session --remote b` moves it there, and
     ends these shells first.

The conservative branch is the SPEC's own: `target` has a defined cwd only when it is
the session's remote, and a second peer would have no directory to be in -- plus a
second reader, since `@file(...)` in that peer's output must be read there. A session per
peer is the cheaper way to reach two, and `set_session --remote` is the one command
that moves one.

The schema is also split per machine now. `background` and `detached` are declared
only on `bash`, where they work, instead of being offered by `remote_bash` and refused
after the model wrote them; they are still refused at run time if written anyway.
`target` is declared only on `remote_bash`.

## The card names the peer

The session's start card (`dynamic_tail`) said the directory and not the machine, so a
model holding `/home/build/project` had no way to know the path was the peer's -- it
would reach for `bash`, and find out by failing. It is a second template now,
`dynamic_tail_remote`, chosen by the same fact that picks the shell:

    Session `x`, harness gongga at `...`, ..., in `/home/build/project` -- a directory
    on remote `build`, not on the machine this server runs on. `remote_bash` is that
    machine's shell; `bash` is this one's.

Two templates rather than one with a clause: a template has no conditional, and a
session on this machine must keep the bytes it has already sent. A third,
`dynamic_tail_remote_unstated`, is the peer with no directory named -- see
`docs/notes/server-prompts.md`. The name comes from
`Shells::remote_name`, the same field `target` is answered with, so the card cannot
describe a machine the shells are not on. A prompt row also records the peer it was
written for (`Prompt.remote`), so `--remote none` in the directory the session left is
still a move -- the card that says "on remote `x`" does not survive it.

## Listing a peer's directory

`list_paths` exists for one caller: a front-end completing `--cwd`. It answers
names and whether each is a directory, never a path — the part already typed
belongs to the caller, and only it knows how the operator spelled it.

The shell it runs in is kept per target and apart from every session's shells,
because it answers while turns are running and a listing must not queue behind
one. The map is released before the peer is awaited: holding it across an answer
would serialise two completions on unrelated remotes.

### What a listing costs, measured

| moment | wall time |
| --- | --- |
| first listing on a fresh target (ssh, runner, shell) | 1.7-2.5s |
| every later listing | 120-160ms |
| the same loop run directly on the peer | 820ms |

Two things follow. A shell is kept open per target, because an SSH round trip per
Tab would be unusable and this makes the cost local. And a listing does not follow
its command with `pwd`: `RemoteShell::run` keeps the session's directory in step,
which a listing never reads, and that `pwd` is a whole extra round trip.

### Why a shell could stall forever, and what ended it

Two defects, both measured rather than reasoned about.

**A dead transport never woke a shell's reader.** `Inner::close` cleared the
request waiters and stopped there, but a shell's reader does not wait on a request:
it waits on its own `ShellQueue`. Nothing else called `ShellQueue::end` except a
deliberate close. So when the connection died, every shell on it waited forever.
`close` now drains `routes` and ends each queue in one pass.

**And the connection could die without anyone noticing.** The `ssh` we start had
`BatchMode` and nothing else. A tunnel that drops silently leaves writes going
into a socket the peer will never answer, and `ssh` blocks on it; the stdout that
EOF would tell us is stuck behind those writes. It now carries
`ServerAliveInterval=15`, `ServerAliveCountMax=3` and `TCPKeepAlive=yes`, so an
unanswered link is ended within about three quarters of a minute and the transport
reports it.

Measured, killing the `ssh` under a live listing shell and asking for the next one:

| | next listing |
| --- | --- |
| before | **361 seconds**, and no error until the socket finally gave up |
| after | **184 milliseconds**, on a fresh transport |

The flakiness did not disappear -- this machine's tunnel is 750-1030ms per ssh --
but a stall is now a recoverable event instead of a connection nothing can detect
and nothing can wake.

### Waiting is not dying, so a listing keeps no clock of its own

A listing used to give up after 8s and call that a stall. That budget was the only
thing telling "still working" from "gone", and it was wrong in both directions: a
cold listing of `C:/Windows/System32` measured **6.6-8.7s** and was cut off while
it was working, and a shell whose peer had gone was reported as a stall rather than
as a death.

The two are different values now, not different durations. `RemoteShell::pump`
answers `Ok` only when both sentinels arrived, and an error when this shell can send
no sentinel ever again: its queue ended (the connection went), the peer said `End`,
or it never connected. `ShellEnded` carries that to the tool, which names it and
replaces the shell. A command that is merely slow is neither: `sleep 12` measured
`run_until_settled` returning after 12.75s with exit 0, and the same command under a
3s deadline still answers `still_running` -- that deadline is the caller's own
(`bash --timeout_ms`), not this one's guess.

So `list_paths` waits until the peer answers, the shell is gone, or the link's own
ceiling (`[remote] answer_timeout_ms`) has passed with nothing said -- and a shell it
lost is redialled once. That ceiling is not a stall verdict about the command: it says
this peer has not spoken for that long, which on a listing of any size is a dead link
rather than a slow one (6.6-8.7s was the slowest cold listing measured). Measured on
the deadline-free listing: six consecutive listings of `C:/Windows/System32`, 6.6s
cold then 1.7s each, none cut off.

## The shell on the peer gets a console nobody sees

The runner on the other machine is started by `ssh`, which has no console to hand
down. A `bash` started there would be given a new visible console on the *peer's*
desktop -- a window on someone else's screen, caused by a tool call here. The
creation flags carry `CREATE_NO_WINDOW` next to the `CREATE_SUSPENDED` the
suspended start already needed, because the standard library replaces the flags
rather than adding to them: the two have to be computed together.
