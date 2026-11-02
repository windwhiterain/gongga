# `crates/server/src/tools/python.rs`

## The protocol

The interpreter is a child process, not an embedded runtime: the server spawns
`python -u -c <driver>` and talks to it one JSON line at a time. A request is
`{"code": ...}`; the answer is `{"ok", "stdout", "stderr"}`. Because the body
travels as JSON, the model never has to put a program through shell quoting, and
`read_file`/`write_file`/`replace` are ordinary Python calls inside the block.

The driver keeps one namespace for the process, so imports, variables and open
files survive between tool calls. A single expression is evaluated and its
`repr` printed; any other block is executed. Standard output and standard error
are captured separately and returned together.

## Restart and timeout

A timeout stops the interpreter rather than interrupting it: the call fails,
the next `python` starts a fresh namespace, and the reply says so. `restart:
true` does the same at the model's request. There is no way to keep the
namespace across a timeout, because a Python process stuck in a loop cannot be
asked politely to give it back.

## Configuration

`[tools.python] program` and `args` select the interpreter; `uv run python` is
`program = "uv"` with `args = ["run", "python"]`. `GONGGA_PYTHON` overrides the
program, and with neither set the server takes `python3`, then `python`, from
PATH. `timeout_ms` and `max_output_chars` are the same knobs as bash's.

## Session state

`SessionInner::pythons` holds the interpreter behind the same session that owns
the shells, so two sessions never share a namespace and a dropped session drops
the process (`kill_on_drop`). It is one interpreter per session; there are no
named Python shells, because the namespace is the state a name would have to
identify.
