# `crates/server/src/remote/`

A session's shell may run on another machine. What crosses the wire is a **runner**:
one stdio program on the peer that owns that machine's shells and answers requests
by id. See `.agents/notes/2026-11-01-remote-is-a-shell-endpoint.md` for the decision.

## `remote::protocol`

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

A resume is a one-permit wake, not a broadcast, because a resume can arrive before
its pipe's pump starts waiting; a missed permit would stall that pipe for good —
which the tail test catches as lost bytes. When a shell ends, its pumps are woken
and unpaused, since the last bytes are exactly what `End` is waiting for.

On the peer, a write that fails cancels the whole run: shells are stopped rather
than left running where their output has nowhere to go, and requests that arrive
are answered with the reason.

Observed platform difference, recorded rather than claimed away: writing to a pipe
whose read end is closed fails at once on unix, and was observed to *block* under
Windows. The closed-reader test is therefore unix-only; the Windows case is bounded
backpressure, not a lost frame.

## Getting the runner onto a peer

The runner ships inside the distribution, so there is nothing extra to build or
install: **`runner` is the second word of the one file**, and the default target
runs exactly that.

1. On the peer, put the distribution on `PATH` under the name `gongga`:

       scp gongga peer:/usr/local/bin/gongga && ssh peer chmod +x /usr/local/bin/gongga

   Nothing else is needed. The file carries `gongga-cli`, `gongga-server`,
   `gongga-tui` and `gongga-runner`, and it dispatches on its first word, so the
   whole remote side is the one file `ssh` already has to be able to execute.

2. A remote in the server's config, and nothing else:

       [remotes.build]
       host = "peer"

   That means `ssh peer -- gongga runner`. `port`, `identity` and `runner` are the
   only other keys; `identity` is an `ssh -i` path on *this* machine, and `runner`
   is the peer's command, spelled in as many words as it takes:

       [remotes.build]
       host = "peer"
       port = 2222
       identity = "C:/keys/build"
       runner = "gongga runner"          # the default, written out
       # runner = "gongga-runner"       # a copy of gongga-runner on the peer's PATH
       # runner = "/opt/gongga/build"   # any other program that speaks the protocol

3. A session on it:

       gongga-cli set_session --session build --remote build --model <provider>/<model>

   `--cwd` is optional and, when given, is a path **on the peer**: the endpoint
   canonicalises it and refuses one it does not have.

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
- `ShellHandle::close` answers from the endpoint's report only.

`DISCONNECT_GRACE` is the one place this file waits on purpose: when the last
owner drops, the child is *not* killed at once. The runner learns of the end from
EOF on its own stdin and ends its shells itself; killing it first would abandon
them, which is the silence a disconnect must not produce.

## `remote::endpoint`

Three kinds of endpoint, and only one of them can come from a config file:

- `Local { program }` — a shell here, exactly as before;
- `Remote { transport, runner }` — `ssh -o BatchMode=yes [-p port] [-i key] host --
  runner`. Nothing weakens host key checking: `BatchMode` only stops a password
  prompt from hanging a turn, and a host that is not in `known_hosts` is refused by
  `ssh` itself, which is the answer a caller wants.
- `Direct { runner }` — a runner process on this machine, which is how a peer is
  exercised without ssh. **Test-only**: `[remotes.<name>]` has `host`, `port`,
  `identity` and `runner`, and parsing can only produce `Endpoint::Remote`, so no
  TOML file reaches `Direct`; `local` and hostless entries are refused at load.

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
  written into `sessions.cwd`, so the next start begins where the endpoint is. A
  local session is unchanged: this machine's `current_dir` and the existing fallback.

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
prompt's index, for `@file:`, and for `@image:` -- nothing infers a machine from how
a path is spelled. A reference in a tool's output is read on the machine that ran the
tool; a reference in a user's message, on the machine the session stands on.
See `.agents/notes/2026-11-02-a-session-has-two-machines.md`.

Still refused, each with a message and never a local fallback:

- `bash` or `remote_bash` with `background: true` on the remote — the job's scratch
  file and completion watcher are this machine's;
- `detached: true` anywhere but this machine — the process would be started here and
  outlive everything asked of the peer;
- a capped output on a remote is not spilled to a file: the path would name this
  machine, so the model is told the tail was dropped instead.

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

The flakiness did not disappear -- this machine's tunnel is 750-1030ms per ssh and
occasionally stalls past the 8s listing deadline -- but a stall is now a bounded,
recoverable event instead of a connection nothing can detect and nothing can wake.

### A stalled link is bounded, and the shell is dropped

A listing that has not answered in 8s is reported as a stall, not as a directory
the peer refused. The shell is then **dropped** rather than drained: the stall is
the link, and a shell kept for the next question would hold every later listing
behind the same stall. Measured before this: one unlucky call took **394 seconds**
and the calls after it queued behind it. Measured after: that call costs its 8s
deadline, the next one dials again (1.6s), and the ones after are back to 120ms.
