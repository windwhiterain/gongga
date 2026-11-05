# tui/command.rs

The commands the window runs itself: every spec `protocol::client_commands()`
publishes -- `set_sticky`, `del_sticky`, `set_render`, `set_render_session`,
`connect_server`, `serve`. A line whose head is one of those never reaches the
command layer.

## Why the window parses them

They act on `cli.toml`, and `cli.toml` is the window's (`docs/notes/tui-state.md`).
Two owners of one file is how an agent's `set_session` came to rewrite the operator's
sticky session; one owner that is not the command layer also means `gongga-cli` has
no state to inherit, which is the whole point of the split.

The catalog is still the server's, and still one: the window's completion is seeded
with `protocol::client_commands()` and delegates every other line to the client's
completer (`Window`, below). So the names, their arguments and their value hints are
published once, by `protocol`, and both sides read that one list.

## What each command answers

The answers are the ones the command layer gave before it stopped owning them,
because they are what an operator's fingers know:

- every answer replaces the command row (SPEC's row is the last thing asked or told,
  so stacking one row per cleared entry would leave only the last readable -- hence
  one row naming *all* the entries a change forgot);
- `set_render` with no argument is a read, not a reset;
- `set_sticky` refuses a type no published argument declares, while a window with no
  greeting still stores one: an empty catalog cannot prove a name wrong;
- `serve` says where to type it, because the process *is* the server.

`connect_server` is the one command whose answer is an address: it saves it, tells
the caller, and `Done::Moved` is how the loop learns to drop the link and repoint.

## Window

The completion wrapper. A line whose head is a client command is completed from the
window's own catalog -- the one seeded with `protocol::client_commands()` -- and every
other line is the server's completer's business. `refresh` pushes the state the
completion needs (sticky names, the session a bare message goes to, the live session
list, the roots) after any change, so a popup and the line it completes never
disagree.

## follows

The published dependencies, read off the catalog: `(kind, sticky_to)` for every
argument that declares one. The window applies SPEC's rule from this list and never
from a table of its own, so a command that declares the dependency -- a script's, one
day -- is followed the same way.
