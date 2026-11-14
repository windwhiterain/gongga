# `crates/skill/src/entry.rs`

## module doc

An entry is a file, and a file is not a program: something has to read its extension or its
`#!` line and decide what starts it. That decision used to live in the command layer, which
was fine while `gongga-cli skill` was the only door. It stopped being fine the moment the
server grew its own runner (SPEC's `run_skill`): the server cannot depend on the command layer, and a second copy
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
exits 49). It is eager there, which costs one probe on a `run_skill` line that never needed
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
hid one would be answering the same question twice. A call names one: `cmd.name` in `run_skill`'s
object, or `function` in the request below. `cmd.args` is that function's **keyword
arguments**, an object -- not positional words, and not flags, because the request already
has a shape. What the function answers is the answer: `None`, a string, an exit code, or a
structure printed as JSON.

There is no `metadata.entry` to keep in step with a file name, no interpreter table to
consult (it is Python), and no path spelling to get wrong -- on every platform the same
thing happens. A function that cannot be filled is refused with its signature rather than a
traceback, so a wrong call reads like a diagnosis.

## The request, and where the contract lives

A package's process is `python <home>/lib/gongga_skill.py` with **no other argv**: the call
is one JSON line on stdin.

    {"skill": "<skill directory, absolute>", "function": "<name>", "args": {...}}

It used to be `python -c "<the whole contract>" <dir> <argv...>`, and that is what this
shape replaced: the source was a command-line payload, which is a thing Windows caps (a
request holding a whole `SessionConfig` is not a shell's business), a thing every process
listing can read, and a thing that made the contract impossible to change without a rebuild.
`skill::request` writes the line in this crate, so the server's `run_skill` and `gongga-cli
skill` feed the contract the same bytes.

The contract file itself is the content repository's `python/gongga_skill.py`, which the
content step installs into the home's `lib/` -- so editing it takes a content push, not a
`cargo build`. `crates/skill` no longer carries a copy: an `include_str!` of it would be a
second answer to "what does this contract say", and the copy in the binary is the one nobody
can edit. A home without `lib/gongga_skill.py` is refused before a process starts, naming
the path.

A package wins over a declared entry: discovery is the point of the form. Everything else
keeps the script rule below, which stays for the skills that arrived before packages did --
and is the fallback on a machine with no Python at all.

## What a package's process is given

`GONGGA_SKILL_LIB` and the session's own environment (`SessionHandle::env`): the first puts
the home's `lib/` -- `gongga_skill.py` and `gongga.py` -- on `sys.path` for the package's own
imports, the second makes `import gongga` reach *this* server as *this* session. Without the
second a skill written in Python would have to shell out to `gongga-cli` for every command --
which is what `subagent` did before packages -- and its `$GONGGA_SESSION` would be the
launcher's, not the caller's.
