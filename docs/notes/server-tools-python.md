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

## Session state

`SessionInner::pythons` holds a map from name to interpreter, behind the same
session that owns the shells. `python` defaults to `default`; another `name`
starts a separate process, `list_python` reports the live ones (pid, program,
cwd) and `del_python` takes names by regex and stops their processes, the same
verbs `list_bash` and `del_bash` provide. A dropped session drops every process
(`kill_on_drop`), so two sessions never share a namespace.
