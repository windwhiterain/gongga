# A session has two machines, and a tool name says which

User's instruction, 2026-11-02: a session was either wholly remote or wholly local,
and the harness was neither honest nor useful in either half. `bash` ran on the peer,
so the harness's own files — `$GONGGA_HOME`, the community clone, the roster, the key —
were reachable only by shipping them to the peer; while `index_skill`, `@skill:` and
the prompt's index read **this** machine and reported on a checkout that was not there.

The fix is not a transport. It is that `bash` and `remote_bash` are two tools:

```
target (now `remote`) absent  →  one set of shells, here. cwd = the session's, here.
target named                  →  two sets: `remote_bash` there, `bash` here.
                                cwd belongs to the remote; the local set starts in
                                this machine's default directory.
```

`check_bash`, `del_bash` and `list_bash` take no machine: one name is one shell, and the
answer says which machine it lives on. So the name space the model already knows is
unchanged, and `default` is whichever machine owns the session's directory.

## What this buys

- **The harness's own files never leave this machine.** `heir`, `subagent`, `evolve`,
  `gongga-cli`, the roster and the key are all `bash`'s side, which was always here.
  A remote session can now use them, and no key is copied to a peer.
- **A reference is read where it was printed.** A tool's output is expanded by the
  machine that ran the tool (`Tool::machine`), a user's message by the machine the
  session stands on. `@file:` stays literal, as before.
- **No guessing from a path.** The old code took a peer path (`/c/...`) to this
  machine's filesystem and found nothing — or, worse, took a Windows spelling and
  found a *different* checkout's `AGENTS.md`, which then rode the prompt's index for
  the life of the session.

## What is refused rather than emulated

`detached` on `remote_bash`: a detached process belongs to this machine's registry, so
it would be started here and outlive everything asked of the peer. A capped output on a
remote is not spilled to a file, because the path would name this machine. Both are
refusals with advice, never a local fallback.

`background` was on that list and came off it on 2026-11-03, by the user's instruction:
the job's file lives in the endpoint's own temporary directory and its end is an event
-- the wrapper's sentinel, or the shell's channel ending. See
`docs/notes/server-remote.md`.

## Compatibility

`--target` is `--remote`; `[targets.<name>]` is `[remotes.<name>]` (the loader aliases
the old table); `sessions.target` is `sessions.remote`, renamed by `migrate/2-to-3.py`.
`local` is not a remote name: naming none is how a session says "this machine", and
`none` is how a remote session comes home.

## What landed, beyond the arrangement

- **The prompt's index follows the placement, not the shell.** `SessionInner.placed`
  is what `set_session --cwd` last said (and, for a session whose remote shell has not
  connected yet, what that shell first reported). A move writes a new prompt row --
  the old row keeps the old directory -- while a `cd` still writes nothing, which is
  the invariant `scripts/prompt_selftest.sh` holds.
- **A peer's files are read through one connection per remote** (`remote::Reader`),
  never through a session's shell: a message is expanded when it is delivered, which
  may be while that session is inside a long command. A dead connection costs one
  re-dial, and the read then goes on.
- **`index_skill` lists what this server can open** and names where a peer checkout's
  own skills are; `@skill:` resolves against the same rows, so a completed name is a
  name that expands.

Verified on real processes: `scripts/remote_selftest.sh` (one verdict line, a
temporary home, the peer from the operator's config) -- 12 checks, 27s, and it goes
red when a remote reference is read from this machine's disk instead.
