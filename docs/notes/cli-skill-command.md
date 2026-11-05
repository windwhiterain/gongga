# `crates/cli/src/skill_command.rs`

## module doc

`gongga-cli skill <name> [args…]` runs what a skill publishes -- the entry its `SKILL.md`
declares, or the function of the package it is. It exists because the
alternative the skills had was to spell their own installed path in prose, and that path
is the one thing about a skill that moves: the home can be another `--home`, the harness
layer is reinstalled from a pin, and the machine's layer is a clone. A reader that copied
a path from a body got a command that worked on the machine that wrote it.

It also exists in the shape it does -- one command, not one trampoline script per skill --
because a name in `<home>/bin` is a name in the shell's namespace: a skill called `git`
would shadow `git` for every `bash` this harness ever runs, and a `bin/` of skill names is
a second package manager's worth of collision surface for a shortcut.

## Where it runs

Handled in `cli::main` before a runtime exists and before a link: what it needs is a
directory on this disk, not the server. That is why "a skill's argv is not this command's
argv" is enforced in `check_flag_order` -- `skill remote add --home x` names the script's
flag, and the order check would otherwise refuse the line as a misplaced global.

`skill ls` is the one verb that can be answered by either side. With `GONGGA_SESSION` set --
a shell the harness started -- it asks the server for the table *that session* would be
offered (`skill <session>`, no `name`), because the project root is the session's checkout
and the command layer's cwd is not it. With no session it walks this machine's roots: a bare
shell is not standing in anything. Both print the same four columns -- name, root,
description, `SKILL.md` -- so a reader and a model are looking at one shape; the server's
answer arrives as the tool's JSON and is rendered here, which is why `ask` exists (one
command, asked quietly: its `cmd_result` and nothing else).

That local route now yields to a server that publishes `skill` (`crates/server/src/tools/
skill.rs`): the greeting's catalog decides, as it does for every other name, and the entry
runs on the session's terms with the output capped and shaped like any other tool result. The
fallback is what the rest of this note is about -- a machine with no server still runs an
entry, because an entry is a file and not a command's work.

The two spellings of the same line differ in one place, on purpose: the server command types
its arguments by the tool's schema, so `skill main web-search '["fetch", "<url>"]'` arrives as
a word list, while this route passes argv as typed and unparsed, as SPEC says.

## dispatch

The verbs are `ls`, `run`, and anything else is a skill name. `run` exists only so a skill
that is itself named `ls` or `run` is reachable: the diagnostic is the common case and a
skill name is the fallback, not the other way round. There is no `which`: what a skill runs
is its `SKILL.md`'s `metadata.entry`, which is a line of the body a reader already has, and
`ls` hands out that `SKILL.md`.

Everything after the name is argv, and a package reads its first word as the function to
call: `skill subagent spawn impl` is `spawn("impl")`. Nothing here knows that -- `argv` is
passed through to the runner, and `crates/skill/src/entry.rs` starts
`python -c <gongga_skill.py> <skill dir> <argv…>`, whose own first argument is the function.
So this layer stays one rule for every skill, and "what does this package publish" is
answered where a package is understood.

## launch

The child inherits this process's three streams, and its exit code is this process's exit
code. A skill's answer *is* its output: `web-search` prints results, `heir` prints the
first lesson it could not deliver, `remote` prints one JSON line. Re-encoding any of that
-- into a row, an event, a JSON wrapper -- would be a second copy of a contract the script
already keeps, and `jq`-ability is the reason those scripts print what they print.

Nothing runs in the skill's directory: the entry is absolute, and the scripts find their
own neighbours from `$BASH_SOURCE`. A `cd` here would make every relative path a skill's
body mentions mean something else than the session's cwd, which is not this command's
business.

## runner

`runner` moved to `crates/skill/src/entry.rs` when the server grew the `skill` tool: the rule
"which program starts this file" has one answer, and the server could not keep a second copy.
What stayed here is the *lookup* -- `which`, the Python probe, PowerShell -- handed to the
crate as `skill::Programs`. The paragraph below is the reasoning that moved with it.

An entry is run by the program its extension names -- `bash` for a shell script, a Python
for a `.py`, PowerShell for `.ps1`, `cmd /c` for a batch file -- and an entry with no
extension is run by the program its own `#!` line names, because Windows does not honour
one. That last case is not hypothetical: `transcript`, `show`, `remote` and `wait_turn` are
all spelled without an extension, and handing one to `CreateProcess` is `os error 193`. So
an extension-less entry is dispatched on the interpreter its first line asks for, `env` or
not, and only a file that asks for nothing is executed as itself.

The path it is handed is spelled for that program, because the two spellings are not
interchangeable: `bash` reads `/c/a/b`, and Python and Windows read `C:\a\b`. Handing a
POSIX spelling to Python is a file it cannot open; handing a Windows spelling to bash
breaks the `dirname "${BASH_SOURCE[0]}"` every one of these scripts starts with.

The Python probe is the six-times-duplicated loop the entry scripts carry (`for candidate
in python3 python py; do … ; done`, because Windows' Store alias sits on `PATH` and exits
49): asked once here, so a skill's body no longer has to explain which interpreter the
machine honours. The scripts keep their own loop, because each stays runnable on its own
-- that is the portable half of the contract, and this command is the shortcut.

## The Python probe does not flash a window

`python()` asks each candidate interpreter to run `import sys`, and `python.exe` is
a console program on Windows. The command layer often has no console of its own --
it is a command in a pipeline, or a program the bundle started -- so the probe
would put a window on the user's desktop for as long as it took to import `sys`.
The probe's creation flags carry `CREATE_NO_WINDOW`, and its stdio is null: it
answers a question, it is not a program anyone reads.
