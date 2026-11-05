# `crates/skill/src/entry.rs`

## module doc

An entry is a file, and a file is not a program: something has to read its extension or its
`#!` line and decide what starts it. That decision used to live in the command layer, which
was fine while `gongga-cli skill` was the only door. It stopped being fine the moment the
server grew a `skill` tool: the server cannot depend on the command layer, and a second copy
of the rule is a second answer to "what runs this file".

So the rule moved here, and only the rule. What it does *not* do is look for programs.

## Programs

`bash`, a Python and a PowerShell arrive already resolved, and every field is an `Option`:
the server has a configured bash and a Python from its roster, the command layer finds them
on PATH, and neither wants to pay for the other's lookups. A `None` is "not found", and the
error naming it is made only when an entry asks for that interpreter -- so a `.sh` entry on a
machine with no Python still runs, and a `.py` entry on a machine with no bash says exactly
which program is missing.

The command layer's Python lookup is the six-times-duplicated probe the scripts carry (`for
candidate in python3 python py; do … ; done`, because Windows' Store alias sits on `PATH` and
exits 49). It is eager there, which costs one probe on a `skill` line that never needed
Python. That was accepted rather than solved: the alternative is the command layer asking
this module which program an entry needs, which is the rule leaking back out.

## runner

An entry is run by the program its extension names -- `bash` for a shell script, a Python
for a `.py`, PowerShell (`-NoProfile -File`) for `.ps1`, `cmd /c` for a batch file -- and an
entry with no extension is run by the program its own `#!` line names, because Windows does
not honour one. That last case is not hypothetical: `transcript`, `show`, `remote` and
`wait_turn` are all spelled without an extension, and handing one to `CreateProcess` is
`os error 193`.

A `#!` naming a program this module knows nothing about (`node`, say) is returned as a bare
name rather than resolved here: the caller's PATH is the thing that answers, and a name that
is not there fails to spawn with its own words. A `#!` naming a shell is `Programs::bash`, even
if the line says `zsh`, because the shell this machine actually has is the configured one.

## spell and words

The path an entry is handed is spelled for the program that reads it, because the two
spellings are not interchangeable: `bash` reads `/c/a/b`, and Python and Windows read
`C:\a\b`. Handing a POSIX spelling to Python is a file it cannot open; handing a Windows
spelling to bash breaks the `dirname "${BASH_SOURCE[0]}"` every one of these scripts starts
with.

`words` is the whole invocation in one place -- the program's own leading arguments
(`-NoProfile -File`), the entry, then the caller's argv -- so the two callers cannot disagree
about their order. Neither caller writes a command line: each hands the words to a spawn that
provides no shell, so a word holding a space, a quote or a `$` arrives as it is.

## The package form

A skill may be a Python package instead of a script: the directory holds `__init__.py`, and
what to run is *discovered* rather than declared. Its public API is its **functions**:
whatever global scope holds under a name that does not start with `_`. `__all__` is not a
second spelling of that -- `SKILL.md` is where the functions are described, and a file that
hid one would be answering the same question twice. A call names one: the first word after
the skill. The rest bind to
that function's parameters in declaration order, or by name with `--name value`, and an
annotation of `int`, `float` or `bool` converts the word. What the function answers is the
answer: `None`, a string, an exit code, or a structure printed as JSON.

There is no `metadata.entry` to keep in step with a file name, no interpreter table to
consult (it is Python), and no path spelling to get wrong -- on every platform the same
thing happens. A function that cannot be filled is refused with its signature rather than a
traceback, so a wrong call reads like a diagnosis.

`python/gongga_skill.py` is that contract, and it is one file with two readers: the harness
carries it with `include_str!` and runs it as `python -c <source> <dir> <argv...>`, and a
Python caller imports it and calls `run(directory, argv)`. The second is why the form exists
-- an agent's Python can use a skill in-process, with the argv as a list and the answer as a
value, instead of re-deriving a command line. Neither reader looks for skills: a name is
resolved by `Roots` first, because "which file does this name open" is one answer and it is
not Python's.

A package wins over a declared entry: discovery is the point of the form. Everything else
keeps the script rule below, which stays for the skills that arrived before packages did --
and is the fallback on a machine with no Python at all.
