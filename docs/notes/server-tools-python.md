# `crates/server/src/tools/python.rs`

## The protocol

The interpreter is a child process, not an embedded runtime: the server spawns
`python -u -c <driver>` and talks to it one JSON line at a time. A request is
`{"code": ...}`; the answer is `{"ok", "stdout", "stderr"}`. Because the body
travels as JSON, the model never has to put a program through shell quoting;
file work is `open()` and `Path` inside the block, not a shell redirection.

Each named interpreter keeps one namespace for its process, so imports,
variables and open files survive between calls to that name. A single expression
is evaluated and its `repr` printed; any other block is executed. Standard output
and standard error are captured separately and returned together.

Every call also takes an optional `description`: it is the tool row's title and
the name an over-long output is spilled under, through the same `ToolContext::cap`
bash uses. The three verbs all declare it, so a call never has an undeclared key.

## Restart and timeout

A timeout stops that interpreter rather than interrupting it: the call fails,
the next `python` to that name starts a fresh namespace, and the reply says so. `restart:
true` does the same at the model's request. There is no way to keep the
namespace across a timeout, because a Python process stuck in a loop cannot be
asked politely to give it back.

## Configuration

`[tools.python] program` and `args` select the interpreter; `uv run python` is
`program = "uv"` with `args = ["run", "python"]`. `GONGGA_PYTHON` overrides the
program, and with neither set the server takes `python3`, then `python`, from
PATH. `timeout_ms` and `max_output_chars` are the same knobs as bash's.

## background, and why the counters live outside the lock

`python` and `remote_python` take `background` exactly as `bash` does, and
`check_python` is `check_bash`'s collector for them. But an interpreter answers one
call at a time, which changes two things a shell never had to face:

- **a foreground call while the name is busy would read the other call's answer.**
  The driver pairs replies with requests by order, not by id, so the second request's
  reply lands on the first reader and both results are silently wrong. So the name is
  refused instead, and the message says to read the running one with `check_python`.
  Measured: the refusal arrives at once, and the background answer still belongs to
  its own call.
- **what decides that must not take the interpreter's lock**, because a background
  call holds it for as long as the code runs. `Interpreter` keeps `jobs`, `pending`
  and the stop handle beside the lock, and `Facts` (machine, pid, program, cwd) is
  fixed at spawn. Measured the wrong way first: reading `machine` through the lock
  made the busy check wait for the running call and then report the name idle, so a
  foreground call went through and 928ms later answered from the wrong request.

`del_python` stops an interpreter through `Control`, never the lock: measured at 6ms
while a `sleep 60` call was running.

The notice is the shell's, reused: `notice_background_end` is one function for both
tools, told what to call the work and where to read it. Measured: *"The background job
1 you started with `remote_python` in `default` has finished successfully after 3.0s."*

## remote_python

The same driver on the session's peer. One `Repl` enum holds either side, so a name is
one interpreter and the other tool's answer to it is refused rather than borrowed --
`\`default` is already a Python on its remote`, measured. The refusal asks for another
`name` first and mentions `del_python` second, because a name is what the caller can
change without ending somebody's work. Its description says only that it is `python` on
the peer; the name space and the rest are the `remote` skill's.

What is *not* shared is how the process is reached, and two measurements decided it:

- **`exec` in the peer's bash does not replace the shell.** It starts a child and
  exits, so the runner sees the shell end and every request is answered with "the
  Python interpreter on the endpoint ended". So the peer starts the program itself:
  `Op::Open` carries `args`, and `--norc --noprofile` stay the shell's business.
- **a name on the peer's `PATH` is not an interpreter.** On a Windows peer `python3`
  is the Store's alias stub: it exits 0 having run nothing, and an empty answer read
  as success is a silent wrong answer. So every candidate is asked a real question
  first (`1 + 1`) and one that cannot answer it is not that machine's Python.

Because `args` is a new meaning on `Open`, the runner protocol went to **2**: a
runner that ignored `args` would start `python` with `--norc`, which is a wrong
answer rather than a missing one. Every peer already installed must be reinstalled,
and the refusal says which versions disagreed -- measured: *"the endpoint's runner
speaks protocol 1, and this server speaks protocol 2"*.

Paths are the interpreter's, and the interpreter is native: a Windows peer's Python
wants `C:/Windows/win.ini`, and answers `[Errno 2]` for `/c/Windows/win.ini` however
well the shell beside it resolves that spelling. Measured both ways.

## Session state

`SessionInner::pythons` holds a map from name to interpreter, behind the same
session that owns the shells. `python` defaults to `default`; another `name`
starts a separate process, `list_python` reports the live ones (pid, program,
cwd) and `del_python` takes names by regex and stops their processes, the same
verbs `list_bash` and `del_bash` provide. A dropped session drops every process
(`kill_on_drop`), so two sessions never share a namespace.

## File operations

`read_file`, `write_file` and `replace` are the same driver with a structured
request: `{"op": "read"|"write"|"replace", ...}`. The path, content, old and new
text are JSON fields; no shell or Python source parses them. Each call starts a
fresh driver and stops it afterwards -- file state is the file, so a persistent
process buys nothing and a long-running interpreter must not block an edit.

The driver runs on this machine, or on the session's remote when the call's
`remote` names it -- the same choice `remote_python` makes, and the same one
machine: a session's directory is on one peer, so a second name is refused. The
remote driver is started by name there,
exactly as `remote_python` starts it, so a peer without Python refuses the call
rather than writing on this machine.

`remote` is offered only to a session that has a remote -- `definitions` drops the
argument otherwise, so a session on one machine is never shown a machine it does not
have. `Tool::parameters` still declares it, because the operator's catalog is built
from the same schema and `argument_error` reads it: a session that writes the argument
anyway is refused by `file_remote`, not by the schema.

Writes go to a temporary file in the target's directory and are moved into place
with `os.replace`; the existing mode is kept. `replace` is literal and, unless
`all` is set, requires exactly one occurrence, so a stale `old` is an error rather
than a silent no-op. `read_file` takes zero-based `offset` and `limit` lines.

## A wait that is cut

The wait is `ToolRegistry::run`'s, so a message or a cancelled turn ends it for
`python` as it does for `bash`. What happens next cannot be `bash`'s answer:
a foreground call reads its own reply line while it holds the interpreter's lock,
and a call that returned without reading it would leave that line in the pipe for
the next caller, which would take it as the answer to a different question. So
the interpreter is ended with the call, and the next call under that name starts
with a fresh namespace. `check_python`'s `wait_ms` is a clock and nothing else --
`wait_idle` no longer watches a token of its own.
