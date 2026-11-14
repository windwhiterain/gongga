# `crates/skill/src/package.rs`

## module doc

A skill runs as one thing: a Python package, and this home's contract. A skill has no second
form -- there is no declared entry, no extension table, no `#!` line to read -- because SPEC
gives the model one door, `run_skill`, and a Python call is what that door runs. The command
layer has no door either: `gongga-cli` passes server commands, and a skill is reached by
`run_skill` and by nothing else.

Three functions and a struct, and the split is the caller's: `is_package` answers whether this
directory is one (the server refuses before it builds anything else), `package_of` answers what
starts it, and `request` writes the line that start reads. Nothing here looks for a program --
that is the caller's half: the server hands `package_of` the single interpreter its config
resolved.

## The package form

The directory holds `__init__.py`, and what to run is *discovered* rather than declared. Its
public API is its **functions**: whatever global scope holds under a name that does not start
with `_`. `__all__` is not a second spelling of that -- `SKILL.md` is where the functions are
described, and a file that hid one would be answering the same question twice. A call names
one: `cmd.name` in `run_skill`'s object. `cmd.args` is that function's **keyword arguments**,
an object -- not positional words, and not flags, because the request already has a shape. What
the function answers is the answer: `None`, a string, an exit code, or a structure printed as
JSON.

There is no `metadata.entry` to keep in step with a file name, no interpreter table to consult
(it is Python), and no path spelling to get wrong. A function that cannot be filled is refused
with its signature rather than a traceback, so a wrong call reads like a diagnosis.

## Package

`python` is the interpreter the caller resolved, and `contract` is this home's
`lib/gongga_skill.py`; the caller runs the pair, with the contract as the only word.
`package_of` refuses a home with no `lib/`, a `lib/` with no contract, and a machine with no
interpreter -- each naming the thing to install or point at, because none of the three is a
skill's fault.

## The request, and where the contract lives

A package's process is `python <home>/lib/gongga_skill.py` with **no other argv**: the call is
one JSON line on stdin.

    {"skill": "<skill directory, absolute>", "function": "<name>", "args": {...}}

It used to be `python -c "<the whole contract>" <dir> <argv...>`, and that is what this shape
replaced: the source was a command-line payload, which is a thing Windows caps (a request
holding a whole `SessionConfig` is not a shell's business), a thing every process listing can
read, and a thing that made the contract impossible to change without a rebuild.
`skill::request` writes the line, so no caller can spell the same bytes differently.

The contract file itself is the content repository's `python/gongga_skill.py`, which the content
step installs into the home's `lib/` -- so editing it takes a content push, not a `cargo build`.
`crates/skill` carries no copy: an `include_str!` of it would be a second answer to "what does
this contract say", and the copy in the binary is the one nobody can edit.

## What a package's process is given

`GONGGA_SKILL_LIB` and the session's own environment (`SessionHandle::env`): the first puts the
home's `lib/` -- `gongga_skill.py` and `gongga.py` -- on `sys.path` for the package's own
imports, the second makes `import gongga` reach *this* server as *this* session. Without the
second a skill written in Python would have to shell out to `gongga-cli` for every command, and
its `$GONGGA_SESSION` would be the launcher's, not the caller's.
