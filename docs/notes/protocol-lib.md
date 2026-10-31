# protocol/lib.rs

## lib

The crate is deliberately dependency-light so the CLI does not inherit the
server's runtime. The wire layers:

* `frame` — the fixed 26-byte datagram header.
* `channel` — a reliable, ordered, fragmented message channel over UDP.
* `message` — the JSON event vocabulary the two peers speak.

## BUILD

One string every program that links this crate carries, so "are the server and the
client one build" is a comparison rather than a guess. It is not the version:
`0.1.0` says which release, `BUILD` says which commit, and two builds of one
release is exactly the case a version cannot answer.

Where it comes from: `build.rs` asks git for the short sha and whether the tree
was dirty, or takes `GONGGA_BUILD` when a release pipeline sets it. The version is
the fallback (`unknown+0.1.0`) where there is no git at all -- a source archive,
a vendored build.

What it watches is the point: `cargo:rerun-if-changed` names the *ref* the commit
moved, not `.git/HEAD`, which reads `ref: refs/heads/x` and is identical before and
after every commit. Watching HEAD would bake yesterday's sha into today's build
and nobody would see it. A worktree keeps `.git` as a file, so `git rev-parse
--git-dir` names the directory instead of an assumed path.
