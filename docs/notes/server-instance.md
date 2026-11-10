# server/instance.rs

## The claim is shared now

`server.lock` used to be claimed by this file. A client home has the same hazard
-- two clients appending to one journal -- so the mechanism moved to
`home_lock::HomeLock` in `crates/lock`, which both take:
`HomeLock::acquire(home, "server.lock", "gongga server")`. See
[`home-lock.md`](home-lock.md) for the claim itself, the stale-pid rule, and why
an empty lock is waited out rather than taken over.

What is left here is the one thing only a server writes: the directory it runs in.

## Why one server per home

Two servers sharing one home is a state nothing here has an answer for: they
share the SQLite file, so each would resume the same pending turn, compress the
same transcript, and drive the same session at the same time. Binding the UDP
port already refuses a second server on the *same* port, but a different port
pointed at the same home slips past that, and both servers would then happily
run the same agent twice.

## record_dir

The claim answers "which process"; this answers "in which directory", written as
`<home>/server.cwd` beside the lock, by the process that just won it.

Nothing else can answer it afterwards. The directory is inherited from whoever
started the server -- `cli::server::command` passes none, so a window's own launch
directory becomes what a new session with no `--cwd` opens in -- and Windows has
no way to read another process's directory: no `wmic` field, no `/proc`. The one
reader is a handover (`evolve`), which starts a successor after the old process is
gone and has to put it in the same directory.

Not removed when the claim is dropped, and a failure to write it is a warning
rather than a refusal: the file is read only while a live claim stands, so a
leftover costs nothing, and a server that cannot write it still serves -- only the
next handover loses the directory.
