# A harness command is a line you type in bash

## The complaint

The agent drove its own server through the `harness` tool, which calls `dispatch`
in-process. That is fast and needs no binary, and it cannot be composed: a tool
call does not go inside a pipe, a `for` loop, a here-doc, a script, or the
`--detached` launcher. Every "do this for each session" therefore cost one model
step per session, and every flow that lives in a script had to keep a second
implementation of the same command.

## What changed

1. **`<home>/bin/gongga`** is written at startup (`shell_command::install`) as a
   two-line `exec` of the CLI's absolute path, and every shell this server starts
   gets a `PATH` whose first entry is that directory. The `PATH` is rebuilt entry
   by entry with `msys_path` -- a native `C:\a;C:\b` handed to Git Bash comes out
   mangled (`C:/c/a`, measured here), so a shell would find no command at all.
2. **`GONGGA_SESSION=<name>`** joins the env of a session's shells and of the
   detached jobs it starts. A shell cannot learn which session owns it any other
   way, and half the commands ask for it by name.
3. **Scripts keep the three `GONGGA_*` variables and their own native `PATH`.**
   A POSIX `PATH` is a shell's business; giving it to a Python interpreter would
   break its own lookups.
4. **`gongga` is the command layer's binary** (`crates/cli/src/bin/gongga.rs`);
   the window is `gongga-tui`. One program, one shape. `cli::parse_argv(name,
   about)` makes each say which it is, so `--help` describes the thing run.
5. **A bare `gongga` prints the catalog** the server sent in its greeting. The
   deleted tool used to answer that question in-process; the list stays the
   server's, never a copy in a front-end, because a script registers commands.
6. **Deleted**: `tools/harness.rs`, `ToolRegistry::with_harness`, the
   `Arc::new_cyclic` that existed only to hand the tool a `Weak<Dispatch>`, and
   `commands::delete_own_session`.

The last one deserves its own sentence. `delete_own_session` deferred a session's
deletion because `finish_delete` awaits the session gate -- which the calling turn
holds -- so deleting yourself from *inside your own turn* would have waited
forever. That deadlock was a property of the in-process call site. On the command
line the call arrives over the link and runs on the queue, where `prepare_delete`
interrupts the turn first and the gate comes free. Nothing had to be deferred
because nothing was holding it.

## What is lost, and what was accepted

- The agent now depends on a binary being reachable. `cli_path()` already resolved
  that and warned when it could not; the warning is the same one the tool was
  written to avoid, and it is now load-bearing again.
- One process and one UDP handshake per command. `2026-10-25` measured ~250ms
  over the CLI against 806µs in-process. That number is the price of the
  composition the tool could not do.
- A command from bash has no caller identity: it is exactly what a script, the
  bridge or a person typing `gongga` sends. `GONGGA_SESSION` is data for the
  agent's own use, not a credential, and the wire carries no caller.

## Evidence

`cargo build --workspace` clean (the two pre-existing dead-code warnings in
`session.rs` and `store.rs` are on `main` too); `cargo test -p gongga-cli -p
gongga-tui -p gongga-server` = 77 + 215 + 73 passing; `comments-check` and
`test-layout-check` 0 violations.

Real link, worktree-local home (`--home .dev/smoke-home`, bind 9713), fake LLM
emitting one `bash` tool call:

```
WHICH=/c/resource/gongga/.worktrees/cmd-in-bash/.dev/smoke-home/bin/gongga
12 command(s); run one with `gongga <command> --<arg> <value>`.
send_message -- Send a message to a session and run a turn.
    takes: --message (required), --session (required)
...
SESSION=prove
CLI=C:\resource\gongga\.worktrees\cmd-in-bash\target\debug\gongga.exe
```

`command -v` resolves to the installed command, the bare call lists what the
server published, and the session name is visible inside the shell. A second run
finished with `gongga cancel` typed from that same shell: turn `Cancelled`,
`server stopped`, `closing shells count=1`, process exit 0 -- which is the shape
the `evolve` swap depends on.

The commands themselves were driven from outside the turn as well: `set_session`,
`send_message`, `get_messages`, `interrupt_session`, `del_session` all answered
over the link against the same home.

## Not done

- The shipped prose (`ship/config/config.toml`, `skills/harness/self-control`,
  `skills/harness/heir`) still teaches the tool. It is the next commit, and until
  the pin moves the running server still reads the old instructions.
- `DESIGN.md`'s account of the 2026-10-25 decision ("the prompt teaches the tool;
  `$GONGGA_CLI` is only for scripts") is now the rejected version of this design.
- Nothing guards an agent from `gongga cancel` while it is mid-command; the tool
  had no guard either, and `evolve` depends on the door being open.
