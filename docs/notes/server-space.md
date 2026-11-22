# server/space.rs

One path space: the machine a session's paths live on, and how a program runs there.

## The one spelling, and what its two halves mean

`remote://<name>/<path>` names a machine and a path on it, and it is the only thing that
names a machine anywhere in a call: `exec`'s and `new_channel`'s `cwd`, a session's
`workspace.cwd`, `read`/`write`/`edit`'s `path` and `list_paths`' `path` are all read by
`machine_of` and `stated` here. There is no argument beside a path that names one.

- **The name** is a peer this machine configures, or `local` / `none` / empty for this
  one. A name nothing configures is a refusal that lists the peers there are -- never a
  quiet fall back to this machine.
- **The path** has to be whole in that machine's spelling (`/home/x`, `C:/x`), because the
  only directory this process knows is on this machine: joining the two would reach the
  right machine with a path from the wrong one. A relative path is therefore spelled bare,
  where the caller's own directory stands.
- **An empty path** (`remote://<name>/`) names the machine and no directory: the machine's
  own starting directory stands there, or the session's own when the machine is its own.
  That is what places a session on a peer without stating where it works yet.

## The seam

A tool takes a path and runs a program. Which machine that path is on used to be the
tool's own business: `read_file` had a local branch and a peer branch, `worktree` called
`git` here and could not exist there, and each new tool had to grow the same fork.
`Space` is that fork moved into one value: a session has exactly one space -- the
machine its directory is on -- and a caller asks it to run a program and gets back the
bytes and the exit status.

```
Space::local()            this machine, no protocol
Space::of(endpoint, name) the endpoint's own machine, always through its runner
```

A space built for a peer also holds the operator's name for it, and `Space::name()` is
that name or `None` for this machine. It is kept here because a path is spelled with the
machine that holds it -- `remote://<name>/…` -- and the space that read the file is the
only thing that knows which machine's file it was: a caller of a read has a path, never a
machine. `None` is this machine and never one of its names, so `remote://local/` stays
the one spelling of a path here.

`run` was the whole surface at first, because the file tools already have a driver: a
request goes to a Python interpreter the *space* starts, and the answer is what that
interpreter printed. What a tool must not do is look at the machine at all. Anything
that needs a second primitive -- a file read that is cheaper than an interpreter, a
listing -- belongs here, next to `run`, never inside the caller.

## One space, one connection

A `Space` holds the peer's connection inside itself (`Arc<Mutex<Option<Runner>>>`), and
`Clone` shares it rather than copying it. So `read_bytes`, `list`, `stat`, `free_space`
and `run` all ride one dialled runner: a message naming twelve files costs one
dial, and the disk report after a tool call costs none.

Before this, a peer was reached two ways: `Space::run` dialled through `pick::runner` on
every call, while `remote::Reader` kept its own connection for file reads. `meta` used the
second, the file tools the first, and a session with a peer therefore held two runners
and dialled once per program it ran. `Reader` is gone; its job is this space's file side
(a `read` loop that advances the cursor, a `redial` that costs one re-dial for the whole
read, a `Drop` that leaves the peer reaping its own shells).

The null `Space::local()` has the same field and never fills it: there is no peer to
dial, and the file primitives take the `std::fs` and `std::process` path.

## Where the run goes

This machine: a `tokio::process::Command` with the child's own `cwd`, its stdin closed
when the caller stated none, and `kill_on_drop` so a caller that stops waiting leaves
nothing running.

A peer: `pick::runner` + `Op::Open`, the shell machinery the runner already has -- a
program with arguments, its output streamed as events, and its `End` carrying the exit
status. No op was added for this: `Open` already means "start this program here", and a
second way to run one would be a second way to get it wrong. The shell is closed when
its output ends, so the peer reaps it.

## No clock

`run` has none. The file tools already carry a configured wait and wrap the call in it;
`git worktree add` has no honest ceiling -- a big checkout is slow and correct -- so a
number here would kill legitimate work. A caller that wants a deadline states it.

## What is not yet here

This machine's space has no notion of a working tree it owns, and the `bash`/`exec`/channel
paths still hold their own session shells: they are long-lived conversations with a
workspace, not one-shot runs, and moving them onto this seam is a separate change.

`meta` reads no source of its own. A reference is resolved through the same `Space` a
tool's path is: `spell` joins the name the way that machine joins paths, `read_bytes`
answers the bytes, and `is_local` says which machine a `@skill(...)` is looked up on. The
three answers `Source` used to know collapsed into one -- a peer whose link cannot be
dialled is still a peer, and its unreadable reference stays literal, exactly as an
unreachable one did before. See `docs/notes/server-meta.md`.

## The Python mirror

The skills run in Python, on this machine, whatever machine the session stands on
(`tools/skill.rs` starts them in `ctx.dir(Machine::Local)`). So the file skill cannot
borrow this `Space`: it has its own, `ship/python/gongga_space.py`, installed to
`<home>/lib` and importable by every skill. Same seam, same rule -- one value decides
the machine, a caller asks it to read bytes, write bytes or run a program and never
asks which machine it is on.

Its primitives are the file side this `Space` has not grown yet: `read_bytes` rides
the runner's `Read`, and `write_bytes` is an `Append` to a temp beside the target and
one `Open`'d `mv`. Not a `cat` through a shell: the runner's `Write` never closes a
shell's stdin, so a program that reads to EOF would wait forever -- the same reason
`run` here only ever fed a driver that reads one line and exits. The bytes move
through the runner's own base64 file I/O, binary-safe; the shell only renames.

The transport wheels (`Peer`, `run`, `hello`, ...) are one copy in that module, which
the `remote` skill imports too, so a shell, a file read and an install reach a peer
the same way. The `file` skill's `read`/`write`/`edit` take `remote`/`cwd` and default
to the session's machine; the matching, BOM and all-or-nothing logic stays local and
is one implementation on either machine.
