# Remote is an endpoint of the shell, not another server

## Decision

A session may name an endpoint. The endpoint says **where a shell runs**, and nothing else:
the home, the database, the credentials, the transcript and the agent loop stay on this
machine. `BashShell` keeps driving one long-lived bash; the only difference is that its
three pipes belong to an `ssh` child instead of to a local `bash` child.

No `target` parameter is added to any tool. The endpoint belongs to the session, the way
`cwd` does, so a model call cannot name another machine -- there is no field to name it in.

## Why this shape

`tools/bash.rs` already speaks a line protocol: write the command to stdin, follow it with
a sentinel that prints the exit code on both stdout and stderr, drain each pipe
incrementally, and treat a missing sentinel as `out_of_sync`. A non-interactive bash fed
by a pipe is the same process whether the other end of that pipe is a local spawn or an
`ssh` channel, so:

- `cd`, exports and shell functions survive across calls **without replaying anything**;
  it is one process, which is what `docs/notes/server-tools-bash.md` already promises;
- exit codes stay exact, because bash itself prints the sentinel;
- `session.cwd()` keeps working, because the default shell reports its own directory and a
  remote bash reports a remote path;
- a dropped connection is a pipe that ended without its sentinel, which is the behaviour
  `out_of_sync` was written for.

The alternative -- a snapshot/replay layer that carries `cwd` and exports in the client and
re-establishes them per call -- is what a one-shot transport needs. Here it would be a
second mechanism for state that one process already holds.

## The endpoint value

    Endpoint = Local  { program: PathBuf }
             | Remote { transport: Reach::Ssh { host, port, identity }, runner }
             | Direct { runner: PathBuf }

`ShellSet::new` takes it instead of a bare `program`, and `Shell` is an enum whose two arms
speak the same sentinel protocol, so every tool is written once:

    Local  => BashShell::spawn(program, cwd, env)
    Remote => RemoteShell::new(endpoint, cwd, door)   // unconnected until its first command
    Direct => RemoteShell::new(endpoint, cwd, door)   // the same, without ssh in front

The local arm is the code that exists today, unchanged, so local behaviour cannot regress.
`Direct` exists so a peer can be exercised without `ssh`, and no config file can produce it:
a `[targets.<name>]` entry parses to `Remote` or is refused.

## The runner, not a bare ssh shell

Driving `ssh host -- bash` directly was rejected while implementing this. Two
pipes into a remote bash cannot be inspected, so an independent kill during a
foreground command would mean a second `ssh` and a pid guess, and the file side
would have to read the *server's* filesystem or nothing. So the peer runs a program
of ours, in the shape of `sftp-server`: `gongga-runner`, started by `ssh` on stdin
and stdout, listening on no port, holding no model, no session, no database. It is
reached as the distribution's second word -- `ssh peer -- gongga runner` -- so a peer
installs the same one file this machine did.

- **requests carry ids**, so a signal, a stat or a file read is answered while a
  shell is inside a long command -- `crates/server/tests/remote/main.rs` asserts
  exactly that, with the shell blocked in a builtin;
- **the endpoint owns its paths**: `open` canonicalises the requested directory on
  the peer and refuses one it does not have, so a server path can never quietly
  become a local fallback;
- **the endpoint owns the shell tree**: it starts each shell in its own group, the
  supervisor task holds the `Child`, and `close` is answered `confirmed` only when
  the group was observed empty. Anything else is `unknown`, never "stopped";
- **output is numbered per shell and stream**, so a client can tell a gap from a
  quiet job without polling.

A Git Bash peer has no group to signal, so a named signal there is the verified tree
kill -- the same verb TERM and KILL mean.

## What changes

1. `session.rs` -- the session row gains `target`; `set_session --target <name>` sets it and
   an unknown name is refused rather than ignored.
2. `session.rs` (`materialise_locked`) -- `cwd.exists()` asks the **local** filesystem, so on a
   remote session it answers about the wrong machine. Existence is a question for the
   endpoint. This is also the existing bug: a vanished directory currently falls back to
   the server's own `current_dir()` and says nothing.
3. `tools/index_skill.rs` (`installed(cwd)`) -- project skills are listed from `<cwd>/.agents/skills`,
   which for a remote session must be listed **through the endpoint**. Otherwise every skill
   the model is handed is a path that does not exist where it runs.
4. `agent.rs` (`image::resolve(&ctx.cwd, ..)`) -- the same: bytes must come back over the
   endpoint, not be read from this filesystem.
5. `ToolContext.output_dir` -- spill files are named in the result and the model is told to
   `sed`/`grep` them. In a remote shell that path is a hole. The spill must land where the
   shell can read it, or the sentence must stop naming a path.
6. `run_background` -- the scratch and pid paths come from `std::env::temp_dir()` on the
   server, and a remote bash writing there writes to a directory nobody will read. The shell
   must report its own temporary directory.

## Lifecycle: a shell's end is decided outside its lock

The first implementation of this had a hole worth recording, because the fix is what
the code now looks like. A shell's `Arc<Mutex<..>>` is held for as long as a command
runs, so `del_bash` and a session's teardown -- which both wanted "the shell" -- could
not reach a shell busy with a `sleep`, and the fallback for the busy case was a local
`kill_tree(pid)` over a **peer's** pid. That is not a slow path: it is a signal aimed at
whatever process happens to have that number here.

So an endpoint shell has a third part beside its handle and its process: a **door**
(`remote/shell.rs`), holding one state -- `Unopened`, `Opening`, `Connected(control)`,
`Orphaned`, `Closing`, `Closed(report)` -- where `control` is a clonable
`ShellControl = runner + shell id`. Three properties come from that being *one* state
rather than three flags:

- **close is single-flight.** The first caller to see `Connected` closes; every later
  caller waits on a notify and is handed the same report. A second `del_bash`, the
  command owner noticing `out_of_sync`, and teardown cannot each close, and none of them
  can answer `confirmed` without the endpoint having said so.
- **retirement and publication are atomic.** A connection that completes after its shell
  was retired comes back from `Door::opened` and is closed by its own opener, which then
  settles the outcome the retiring caller is waiting for. A command that arrives for an
  already-retired shell is refused, so no peer-side shell is ever left with no owner.
- **an unobserved connection is never called stopped.** A shell that never opened
  anything reports `confirmed` with nothing to end; a shell still `Opening` is waited
  for; a close that does not report in time is `unknown` with the reason.

Only a **local** pid is kept, and only a local pid is ever given to `kill_tree`. The
peer's pid travels in the control for one purpose: saying what was ended.

`Discarded` (the value a set gives back when it takes a shell out) still ends its shell
in a spawned task if it is dropped without being asked, which is the net under an owner
that forgot. It can only log; a caller that asked already has its answer.

## A Git Bash peer ends a shell through its job object

`taskkill /T` was observed leaving a shell's work alive: `bash -c 'sleep 30'` runs
`sleep` in a Windows process whose parent has already gone, so a walk of the process
tree finds the bash tree and misses the child that matters. The local shell had already
solved this with a job object (`JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`), so the runner does
the same: the shell joins a job right after `spawn`, `close` terminates the job, and
`ActiveProcesses == 0` is the proof -- membership follows re-parented descendants, which
is exactly what a tree walk cannot do. On POSIX the existing group protocol
(TERM, grace, KILL, re-observe) is unchanged.

## Flow control and the tail

Two pipes over one connection mean a reader that blocks costs every other shell. Each
shell therefore has a window of frames on the client; filling it sends `Op::Pause`, and
the peer's pump waits instead of reading on, so nothing is dropped and nothing grows
without bound. `Op::Resume` is a single permit, not a broadcast, because a resume can
arrive before its pipe is waiting -- a missed permit used to stall a pump and lose the
tail, which the 2 MB-per-pipe slow-read test catches. `End` is emitted only after both
pipes have signalled that their last byte was written, so a client that stops reading at
`End` has read everything. A failed peer write cancels the run rather than leaving shells
whose output has nowhere to go.

## Distribution: the runner is a word, not a second program

`gongga-runner` has to exist on the *peer*, and asking an operator to build and install a
second program there is a deployment story nobody would follow. The distribution installs
one file and dispatches on its first word, so:

- the bundle embeds `gongga-runner` beside `gongga-cli`, `gongga-server` and `gongga-tui`;
- `gongga runner` is that program, and `runner = "gongga runner"` is the **default**, so a
  peer needs only the same file on its `PATH`;
- the command layer also places `gongga-runner` in `<home>/bin` when it sits beside it,
  which is the copy to `scp` to a peer that cannot take the bundle. A target names it with
  `runner = "gongga-runner"`.

`runner` stays configurable for a peer that keeps the program elsewhere, and the command
is split into words so the two spellings are the same mechanism. It is an internal stdio
program: no subcommand of its own, no help text to learn, nothing a person is meant to
type for their own work. Cross-compilation is not implied anywhere -- a peer runs its own
architecture's build, and a protocol mismatch is refused at the greeting.

## Refused, for now

Each of these is a clear refusal, never a local fallback, and each disappears when the
endpoint file side is finished: `bash` with `background: true`, `bash` with
`detached: true`, `index_skill`, `@skill:` and `@image:` in a message, pictures a tool
output names, and a path in a capped-output notice. `@file:` stays literal on purpose: it
is a path for the endpoint to read.

## Rejected alternatives

- **`ssh host -- bash --norc` with no program of our own.** Two pipes into a remote bash
  cannot be inspected: an independent kill during a foreground command would mean a
  second `ssh` and a pid guess, and the file side would have to read this machine's
  filesystem or nothing. Replaced by the runner.
- **Ending a peer shell by writing a line to its stdin.** It only works when the shell is
  not already inside a command -- exactly when a teardown is needed -- and it made the
  kill path share the lock the command holds.
- **`setsid` plus a death-watch ssh per background job** (`while kill -0 $pid; do sleep 1;
  done`). One connection per job, on a machine nobody watches, to replace a file read the
  runner already has.
- **Snapshot/replay of `cwd` and exports per call.** It is what a one-shot transport
  needs; here one process already holds that state.
- **A `target` argument on each tool.** The endpoint belongs to the session the way `cwd`
  does, so a model call cannot name another machine -- there is no field to name it in.
- **A second server on the peer.** It would need the home, the database and the
  credentials, and would make two agents out of one; everything but the shell stays here.

## Milestone

Committed: the transport (protocol, runner, client, five process tests), flow control and
the `End` ordering, and the session integration -- target rows, `set_session --target`,
the endpoint's directory, the door, and the refusals above. Next: the runner-backed file
side (project skills, images into local blobs, endpoint-local spill), and the
`del_bash`/teardown wording for what the endpoint confirmed.

## Verification

- L1: the local arm is the existing tests; `cargo test --workspace` must stay green before
  anything else is written.
- L2: mutation -- restore the old `cwd.exists()` fallback, and drop the hand-back of a
  connection that arrived after retirement, and confirm a test goes red for each.
- L3: one scripted run against a real host: temporary home, port from the kernel, key passed
  explicitly, readiness by dial, `trap` cleanup, one verdict line. It asserts four things
  only -- `cd` survives a call, an export survives a call, an exit code survives a call, and
  `del_bash` leaves nothing running on the peer. **Not yet run**: everything so far is
  `Endpoint::Direct`, which is the same runner without `ssh` in front of it.

## Open questions

- Whether the endpoint also owns the sandbox and the terminal, or only the shell. This note
  is deliberately the shell alone: the other two can follow the same seam later.
- Whether a session's endpoint may change after it exists. `cwd` may; an endpoint does not:
  its shells may already be on that peer, a lazily created remote shell has no pid to
  compare, and retired shells outlive their names. The row is written once, by the only
  call that creates a session, so it is the endpoint of every shell that session has.
