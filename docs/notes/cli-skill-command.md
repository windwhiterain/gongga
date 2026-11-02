# `crates/cli/src/skill_command.rs`

## module doc

`gongga-cli skill <name> [args…]` runs the entry a skill declares. It exists because the
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

## dispatch

The verbs are `ls`, `which`, `run`, and anything else is a skill name. `run` exists only
so a skill that is itself named `ls` or `which` is reachable: the two diagnostics are the
common case and a skill name is the fallback, not the other way round.

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
