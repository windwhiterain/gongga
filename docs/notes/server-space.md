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
