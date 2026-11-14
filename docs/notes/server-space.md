# server/space.rs

One path space: the machine a session's paths live on, and how a program runs there.

## The seam

A tool takes a path and runs a program. Which machine that path is on used to be the
tool's own business: `read_file` had a local branch and a peer branch, `worktree` called
`git` here and could not exist there, and each new tool had to grow the same fork.
`Space` is that fork moved into one value: a session has exactly one space -- the
machine its directory is on -- and a caller asks it to run a program and gets back the
bytes and the exit status.

```
Space::local()            this machine, no protocol
Space::of(endpoint)       the endpoint's own machine, always through its runner
```

`run` is the whole surface so far, because the file tools already have a driver: a
request goes to a Python interpreter the *space* starts, and the answer is what that
interpreter printed. What a tool must not do is look at the machine at all. Anything
that needs a second primitive -- a file read that is cheaper than an interpreter, a
listing -- belongs here, next to `run`, never inside the caller.

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

The `bash`/`exec`/channel paths still hold their own session shells: they are long-lived
conversations with a workspace, not one-shot runs, and moving them onto this seam is a
separate change. `meta::Source` reads a reference's bytes, and knows three answers
(`Local`, `Remote(reader)`, `Unreachable`) where `Space` knows two -- a reference names
its machine in the text, so it can point at one the session does not stand on.

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
