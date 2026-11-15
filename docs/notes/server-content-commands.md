# server/content_commands.rs

## content_commands.rs

The commands the *content* declares, published by this server like its own.

`ship/ship.toml` names a function of one of its skills, and the harness offers it as a
command of that name: `write` is `file`'s `write`, reachable exactly as
`send_message` is -- `gongga-cli '{"name": "write", "args": {...}}'`, a tool row with
`cmd = "write"`, a client's completion. Before this, a skill's functions were only
reachable through `run_skill` and its generic `cmd` schema, which is a schema that says
nothing: a client cannot complete `path`, and nothing refuses a call that forgot it.

The declaration carries three words and no arguments. What a call carries is the
function's signature, asked of the package at start
(`{"skill": "<dir>", "manifest": true}` -> `{"functions": {...}}`) and answered by
`python/gongga_skill.py` out of `inspect.signature`. So an argument added to the
function is an argument of the command, and one removed stops being offered: there is no
second list to drift.

## one source, four parts

`Dispatch::catalog` builds them in this order: the server's own commands, the declared
ones, one per tool, and whatever scripts registered. A declared name that the server, a
script, or a tool that implements its own work already answers is *not* published --
those are code, and a declaration may not take a name this build answers itself. It is
skipped rather than reported per greeting, because the name it wanted is right there in
the same list.

A name declared here *is* also a tool name: the config row `[tool.write]` with
`cmd = "write"` is a forwarding alias (SPEC's `fn <tool>(**args)` is
`gongga_cli(Cmd{name: <tool>, args})`). A forwarding row is never published as a command
of its own (`server-tool-cmd.md`), so the declared entry is the whole of the name: the
arguments Python answers for, and no `session` a tool row would add.

That is also why dispatch runs the declared route *before* the tool arm: a row that
forwards to a name would otherwise hand the call back to the command layer, which would
find the tool again -- one call, no end.

## Prose from the row, shape from Python

The catalog entry is completed and validated by one client-side engine, so it wants what
a row says and Python does not: a sentence per argument, a value's shape, an order on an
operator's line. Those are joined by name from the row of the same name, and only those:
`required` is the manifest's, never the row's -- the row may be edited by whoever runs
the machine, and a required argument is a fact about the function.

## The wire to the package

One subprocess per declared *package*, not per command, and only when the fingerprint
moved: the content commit, the declarations, and each declared package's `__init__.py`
(size and mtime). A greeting is built while a client waits, so the wait is bounded
(`MANIFEST_DEADLINE`); a call is not, once the command exists.

A package that refuses, times out, or answers something that is not a manifest makes
*its* command absent and the reason a `tracing::warn!` line -- the same shape the script
registry has, and for the same reason: one content mistake must not cost the machine
every other command. Nothing is published for a name whose skill or function does not
answer, which is what makes a typo in a declaration visible instead of a command that
exists and fails when called.

## Route

`route(name)` is what dispatch asks after `resolve`, and it answers only what the last
`commands()` published. The two are read from one `State` under one lock, so the greeting
a client holds and the routing a call takes cannot disagree about which names exist.

## What it runs

`declared_command` builds `run_skill`'s own call (`id` is the skill, `cmd` is the
function and the arguments) and hands it to `run_skill`: one implementation of "run a
package skill", the session's shells and directory, the same output cap. A declared
command that needed its own runner would be a second door into the same package.
