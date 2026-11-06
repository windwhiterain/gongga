# `crates/server/src/worker.rs`

## worker (module doc)

SPEC says a `detached` job "lives across server restart, adopted when server restart".
A tool call is a future, and a future cannot outlive the process that is polling it, so
the only way to keep that promise is for the call to *be* a process. A worker is that
process: the same binary, started with one request file, owning exactly one call.

Nothing of the server is reachable from it on purpose. It reads the home's config and
opens the home's database, because those are facts about the machine rather than about
the session that is gone -- and no shells, no interpreters, no jobs, because those
belonged to the process the caller was talking to.

## Request

`explore` rides with the request because the worker builds its own `ToolContext` and
runs the same `ToolRegistry::run`: a mode the request did not spell would be a way to
refuse a write in the server and allow it in the process the server started. Only the
refusal is the worker's -- its snapshot is its own and means nothing to the session, so
the look that follows the call is taken by the server when it collects the answer.


Everything the call needs is in the file: the home, the session and the call it answers
for, the tool and its arguments, the directory and environment the session gave its
programs, and where the answer goes. The environment is carried rather than recomputed
because it is what the caller's own programs see, and a worker that recalculated it
would be a second opinion about one session.

The answer path is inside the request, so the worker does not have to be told how to name
anything: the server names it, and both sides agree by construction.

## Answer

Written to a temporary file and renamed into place. That is the whole of "it outlived the
server": the server reads this file precisely when it cannot ask the worker anything --
after a restart, an answer is all that is left of a process that has ended.

An answer that does not parse is treated as no answer. The alternative would be to guess
at half a file, and a job's ending is the one thing that must not be guessed.

## call

The runtime is a current-thread one: one call, nothing concurrent. `Config::load_with_home`
is the same loader the server uses, so a worker and its server agree about a tool's
sentences and its ceiling.

The database is best-effort. A worker whose home's database will not open (a copy that is
being replaced, a lock that outlived a crash) falls back to an in-memory one and runs the
call anyway: what the database is used for here is a failure note for the disk monitor, and
losing that is smaller than losing the answer. See `docs/notes/server-disk.md`.

`pylib::install` is called for the same reason the server calls it at start: a `skill`
whose Python imports `gongga_skill` finds it in the home, and the worker is the process
that runs that skill.

## What a worker cannot be given

A tool that acts on what the server holds -- its shells, its interpreters, its jobs, its
rows -- has nothing to do in a process that holds none of them. `Tool::detached_carrier`
answers `Carried::Here` for those, and the call is refused with a sentence rather than
answered with work that would silently do nothing. The five that answer `Worker` are
`exec`, `read_file`, `write_file`, `replace` and `skill`: each is one program or one file,
and each is done by the time it answers.
