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

    Endpoint = Local { program: PathBuf }
             | Ssh { host, user, port, identity, program }

`ShellSet::new` takes it instead of a bare `program`, and `BashShell::spawn` matches once:

    Local => Command::new(program).args(["--norc", "--noprofile"]) ...
    Ssh   => Command::new("ssh").args([host, "--", "bash --norc --noprofile"]) ...

The local arm is the code that exists today, unchanged, so local behaviour cannot regress.

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

## What breaks, and how

### `del_bash` cannot kill a tree it cannot see

Locally a discarded handle is a dead tree: Windows via `ProcessJob` (`KILL_ON_JOB_CLOSE`),
Unix via `process_group(0)`. Over ssh there is no equivalent -- a client that disconnects
does not take the remote tree with it, and `nohup`, `setsid`, `disown` and reparented
grandchildren all survive. Killing the local `ssh` child is not killing the job.

So a remote endpoint needs one more primitive: **exit and check**.

- the remote bash is started under `setsid`, so it is its own process group;
- the exit path writes one line to its stdin asking it to end its own group
  (`kill -- -$$` on POSIX, a `taskkill /T /F /PID <winpid>` for a Git Bash peer);
- the client waits for EOF and then verifies; a shell that is still there is **reported**,
  never counted as deleted.

`del_bash`'s contract does not change: the same regex, the same names, the same "naming it
again starts a fresh shell". Only the mechanism differs, and the tool still says so when it
failed.

### `tail_background` reads a file, not a shell

`tail_background` works because it bypasses the shell: it reads the scratch file and the
pid file straight off the filesystem, which is why it can run while a foreground command
holds the shell. Over the wire that shortcut is gone, and polling every 150 ms at one round
trip per call is not viable. Two changes:

- the completion signal becomes one lightweight ssh per job that waits on the job and
  exits (`while kill -0 $pid; do sleep 1; done`), so an idle job costs nothing and needs no
  polling task;
- the body is read **on demand**, when `check_bash` is called, by streaming the new bytes
  from the scratch file.

`pending: Arc<Mutex<String>>` -- the unread buffer behind `check_bash` -- stays exactly as
it is: it is server memory, and it is not the shell's.

## Failure semantics

No new vocabulary:

- a connection that dies mid-command is a pipe that ended without a sentinel: `out_of_sync`,
  the shell is replaced, the session is told;
- a command is **never replayed**, because a replay could run it twice;
- an endpoint that is unreachable answers with the transport's error, not with a local shell.

The model's own rule does not change either: one name is one process, so inventing a name
per command leaks a process per command. Over a wire that leak is on a machine the session
can no longer see, which is why "exit and check" is not optional on the remote arm.

## Verification

- L1: the local arm is the existing tests; `cargo test --workspace` must stay green before
  anything else is written.
- L2: mutation -- restore the old `cwd.exists()` fallback and confirm a test goes red.
- L3: one scripted run against a real host: temporary home, port from the kernel, key passed
  explicitly, readiness by dial, `trap` cleanup, one verdict line. It asserts four things
  only -- `cd` survives a call, an export survives a call, an exit code survives a call, and
  `del_bash` leaves nothing running on the peer.

## Open questions

- Whether the endpoint also owns the sandbox and the terminal, or only the shell. This note
  is deliberately the shell alone: the other two can follow the same seam later.
- Whether a session's endpoint may change after it exists. `cwd` may; an endpoint probably
  should not, since the shells already running are on the old one.
