# `crates/server/src/tools/skill.rs`

## module doc

One skill, run: the entry its `SKILL.md` declares, or the function the package it is
publishes. There is no `skill` tool and no `skill` command -- SPEC's `## Python Cmd`
gives the namespace two commands, `ls_skill` and `run_skill`, and this module is what
`run_skill` calls. The table `ls_skill` prints is the same walk, for the reason that
matters to a reader: a row's `id` is what `run_skill` takes, so a path one lists is a
path the other runs.

## Three shapes, one door

A skill is a directory holding a `SKILL.md`, and what else is in it decides how it
runs. `Skills::run` is the one place that decides:

| shape | what makes one | what runs |
| --- | --- | --- |
| **body** | only a `SKILL.md` | nothing: refused, naming the path to read. A body is prose, and prose is read. |
| **entry** | `metadata.entry:` names a file in that directory | that file, with the call's words as its argv |
| **package** | an `__init__.py` in that directory | the home's `gongga_skill.py` imports it and calls the function the call names, with its keyword arguments |

Both an entry and an `__init__.py` can be present. The package wins there, and
`crates/skill/src/entry.rs` says why: discovering the functions is the point of that
form. A skill that wants one answer declares one form.

## The call

`run_skill`'s `cmd` is SPEC's `Cmd := {name, args}`, and what `args` *is* depends on the
form the skill has -- so this module reads the call once and hands each form its own
reading, instead of flattening everything into one list of words:

| form | `name` | `args` | what the skill receives |
| --- | --- | --- | --- |
| **package** | the function it publishes | keyword arguments, an object | one JSON line on stdin, `{"skill": ..., "function": ..., "args": ...}` |
| **entry** | its first word | an object as `--key value`, or a list as written | the call as argv |

An `args` list on a package is refused, naming the shape: a function's arguments are
keyword arguments, and position was the entry form's rule. Nothing there is a shell
command line: for an entry no shell stands between the call and the program, so a word
holding a space, a quote or a `$` arrives whole; for a package nothing a caller wrote is
on the command line at all.

A bare word is the whole call, and a string starting with `{` is an object, because an
operator's line spells `cmd` as text.

## What it answers

What the skill printed, then `[exit status N]`, and `ok` is `code == 0` -- the skill's
answer *is* the call's answer, unparsed. Re-encoding it (into JSON, into rows) would be
a second copy of a contract every one of these scripts already keeps, and `jq`-ability
is why they print what they print. The cap is the shared one: over `max_output_chars`
the text is spilled to a file and the answer is that file's name.

A package's return value is rendered by `gongga_skill.py`: `None` prints nothing, a
string prints as it is, an `int` is an exit code, anything else prints as JSON.

## Where it runs

On this machine, in the session's directory, whatever machine the session works on. The
entry is a file this server can open -- the checkout of a peer session is not in the
roots at all, so a `remote` session runs the home's and the machine's skills and is
never handed a path it cannot read. The package form is handed `GONGGA_SKILL_LIB`, so a
skill's own `import gongga` resolves against the home's client; its contract file is
`<home>/lib/gongga_skill.py`, which the content repository installs.

A session whose own directory is on a peer has none *here*, so the entry stands in this
server's own directory (`tools::standing_dir`): an entry skill has no `cwd` argument, so a
directory it cannot stand in is a call that cannot be made, not a failure to report. See
`server-tools-words.md`, "The directory a program stands in".

The run is the session's work: it holds the turn (`tool_cmd::context` is the same
context a tool call gets), its output is capped and spilled the same way, and
`explore::after_job` looks at the workspace afterwards as it does after a shell -- a
skill's entry is a program of the caller's choosing.

## The three roots

`installed_for` walks three roots, nearest first, and `@skill(...)` expansion, `ls_skill`
and `run_skill` all read that one walk:

| bucket | whose | where |
| --- | --- | --- |
| `project` | this checkout | the session's `.agents/skills` |
| `harness` | the content repository | the home's `skills` |
| `general` | this machine | `~/.agents/skills` |

The project layer needs the session *on this machine*: a session whose directory is on a
peer gets the home's and the machine's rows, and one sentence saying that checkout's
skills are that machine's files to `ls` and `cat` over `remote_bash`.

A name in two roots is two rows and two ids. That is the reason `run_skill` takes an
`id` and never a bare name: a path names one row, a name may name two.
