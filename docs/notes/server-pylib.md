# `crates/server/src/pylib.rs`

## module doc

The harness's Python surface is two files, and both are installed under `<home>/lib` at
startup: `gongga_skill.py` (the contract a package skill is run through) and `gongga.py` (a
client for this server). They are installed rather than shipped in the binary's directory
because a home is the thing that moves -- `--home`, a bundle's payload, a test's temporary
directory -- and a path a caller has to guess is the thing this repository keeps failing at.

## One file, several readers

`gongga_skill.py` is carried by `include_str!` in two crates: the skill crate uses it as the
`-c` driver of a package skill, and this installer writes the same bytes where a session's
Python can import it. That is one file with two readers, not two files that agree -- the
build fails if the path is wrong, which is exactly what happened when the module moved and
one `include_str!` still pointed at the old directory.

## Written only when it differs

A restart rewrites nothing that is already right: a caller may be reading the file, and a
mtime that changes for no reason makes every "did the installer run" question unanswerable.

## Who gets this directory on `sys.path`

Three callers, and each says so in the child's environment rather than by copying files:

- the `python` tool, through `GONGGA_SKILL_LIB` on the interpreter it starts;
- `skill`, on the entry process it starts -- so a package's own `import gongga` resolves
  from inside the package, not only from the driver's scope;
- the command layer's `skill <name>`, when `--home`'s `lib/` exists. It writes nothing
  there: a home that never ran a server has no `lib/`, and that home's entry simply has
  no client to import.

`gongga_skill.py` itself puts `GONGGA_SKILL_LIB` on `sys.path` at import time, so the
driver and an imported package agree about where `gongga.py` is.
