# `crates/server/src/tools/skill.rs`

## module doc

One skill, run: the function the Python package it is publishes, or the one refusal
naming what the directory would have to become. There is no `skill` tool and no `skill` command -- SPEC's `## Python Cmd`
gives the namespace two commands, `ls_skill` and `run_skill`, and this module is what
`run_skill` calls. The table `ls_skill` prints is the same walk, for the reason that
matters to a reader: a row's `id` is what `run_skill` takes, so a path one lists is a
path the other runs.

## Two shapes, one door

A skill is a directory holding a `SKILL.md`, and what else is in it decides whether the
model can call it. `Skills::run` is the one place that decides:

| shape | what makes one | what `run_skill` does |
| --- | --- | --- |
| **body** | only a `SKILL.md` | refuses: a body is prose, and prose is read |
| **package** | an `__init__.py` in that directory | runs it: the home's `gongga_skill.py` imports it and calls the function the call names, with its keyword arguments |

The refusal is one sentence for both, and it is not about a file being missing: a skill runs
only as a Python call, so it says what makes one -- a directory holding `__init__.py`, whose
public functions are the names `run_skill` takes -- and where an implementation that is not
Python goes (inside one of those functions). That is the change a model can make; a sentence
that offered a shell door would name a command this harness no longer has.

There is no third shape and no second door. `metadata.entry` is not read on any path, here or
in `crates/skill`: the command line stopped running entries too, because `gongga-cli` passes
server commands and a skill is reached by `run_skill` alone.

## The call

`run_skill`'s `cmd` is SPEC's `Cmd := {name, args}`: `name` is the function the package
publishes, and `args` is that function's keyword arguments, an object. What the skill
receives is one JSON line on stdin, `{"skill": ..., "function": ..., "args": ...}` --
nothing a caller wrote is on a command line at all.

An `args` list is refused, naming the shape: a function's arguments are keyword arguments,
and a call has a shape rather than a position. `tools::skill` spells one word,
the contract, and never an argv.

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
skill is a directory this server can open -- the checkout of a peer session is not in the
roots at all, so a `remote` session runs the home's and the machine's skills and is
never handed a path it cannot read. The package is handed `GONGGA_SKILL_LIB`, so a
skill's own `import gongga` resolves against the home's client; its contract file is
the content repository's `gongga_skill.py`, read in the checkout
(`<home>/ship/python/gongga_skill.py` under the shipped mapping).

A session whose own directory is on a peer has none *here*, so the process stands in this
server's own directory (`tools::standing_dir`): a package takes no `cwd` argument, so a
directory it cannot stand in is a call that cannot be made, not a failure to report. See
`server-tools-words.md`, "The directory a program stands in".

The run is the session's work: it holds the turn (`tool_cmd::context` is the same
context a tool call gets), its output is capped and spilled the same way, and
`explore::after_job` looks at the workspace afterwards as it does after a shell -- a
skill is a program of the caller's choosing.

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

`roots` is that construction, so a caller with no session can ask the same question: the
catalog is built before any client is greeted, and the command a piece of content
declares is a function of the home's or the machine's skills, never of one checkout's.

## What proves it

`bash scripts/skill_call_selftest.sh` runs both shapes against one real server on a
temporary home: the package's function answering with the text it was given and the
`gongga` it imported, and the refusal a directory gets -- whether it is a body or declares an
entry in its frontmatter -- naming the package to make. It is also where
"`import gongga` resolves from the home's `lib/`" is measured rather than assumed.
