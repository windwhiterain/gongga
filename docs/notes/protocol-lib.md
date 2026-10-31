# protocol/lib.rs

## lib

The crate is deliberately dependency-light so the CLI does not inherit the
server's runtime. The wire layers:

* `frame` — the fixed 26-byte datagram header.
* `channel` — a reliable, ordered, fragmented message channel over UDP.
* `message` — the JSON event vocabulary the two peers speak.

## BUILD

The commit, and the only identity there is: every program that links this crate
carries it, so "are these one build" is a comparison rather than a guess, and a
bug report carries the answer.

`0.1.0` is deliberately *not* part of it. It is a release number that cannot tell
two builds of that release apart, so printing it beside the commit would be a
second thing to read and a weaker answer than the one next to it. Cargo still
needs a version in the manifest; nothing prints it.

Where it comes from: `build.rs` asks git for the short sha and whether the tree
was dirty, or takes `GONGGA_BUILD` when a release pipeline sets it. With no git at
all -- a source archive, a vendored build -- it is `unknown`, and that is the
honest answer rather than a version pretending to be one.

What it watches is the point: `cargo:rerun-if-changed` names the *ref* the commit
moved, not `.git/HEAD`, which reads `ref: refs/heads/x` and is identical before and
after every commit. Watching HEAD would bake yesterday's sha into today's build
and nobody would see it. A worktree keeps `.git` as a file, so `git rev-parse
--git-dir` names the directory instead of an assumed path.

## PROTOCOL_VERSION, and why it is not BUILD

Two numbers, two questions, and only one of them can be the commit:

| question | answer |
| --- | --- |
| is this the same code | `BUILD` (the commit) |
| can these two talk | `PROTOCOL_VERSION` |

A harness is split across a network on purpose: the window on a laptop, the
server on the box that has the keys and the database. Upgrading one end is the
normal case, not an edge one, and it is safe exactly when the wire did not
change. Folding the two into one -- "same commit or refuse" -- would make every
server upgrade break every client, which is what `--client_only` and
`connect_server` exist to avoid.

So `BUILD` is compared and reported, and `PROTOCOL_VERSION` is what a refusal
would key on if one were ever needed. Today nothing refuses; the protocol is at
version 1 and the field is carried, which is the cheapest place to start.
