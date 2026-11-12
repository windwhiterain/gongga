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
| **package** | an `__init__.py` in that directory | `gongga_skill.py` imports it and binds the words to the function the first one names |

Both an entry and an `__init__.py` can be present. The package wins there, and
`crates/skill/src/entry.rs` says why: discovering the functions is the point of that
form. A skill that wants one answer declares one form.

## The words

`tools::skill::call_words` reads SPEC's `Cmd := {name, args}` into argv, and nothing
there is a command line: no shell stands between the call and the program, so a word
holding a space, a quote or a `$` arrives whole. Two spellings of `args` end in the
same argv:

- an **object**, `{"tier": "flash"}`, is `--tier flash` -- two words per entry. That is
  the contract `gongga_skill.py` binds to a parameter by name, which is why the order of
  the object does not matter.
- a **list**, `["pick_config", "--tier", "flash"]`, is those words as written -- how an
  entry takes a subcommand and its flags, and how a package takes its arguments by
  position.

The name comes first, then the args. For a package that is the function's name; for an
entry it is just the first word the script sees.

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
skill's own `import gongga` resolves against the home's client.

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
