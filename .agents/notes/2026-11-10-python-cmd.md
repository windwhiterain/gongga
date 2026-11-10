# 2026-11-10 — SPEC's `## Python Cmd`: skills get a path, and two commands

The last pass left this section as Q6, "not started ... a package-shaped change (a wheel,
discovery, the `ServerScript` door) and wants its own session". This is that session, and
the finding is that most of the package was already there: `python/gongga.py` is a
complete client whose `__getattr__` puts *every* published command on a connection, and
`python/gongga_skill.py` is the contract a package skill is run through. What was missing
was two names in the catalog and the spelling that makes them usable from a skill.

## A skill's path

`Skill := path` in SPEC, so `skill::Skill::id` is `<bucket>/<name>`: `project/evolve`,
`harness/heir`, `general/polity`. Three things follow from writing it down once:

- `ls_skill(path, session)` filters with `commands::under`, so `harness` is every skill the
  home installed, `harness/heir` is one, and `har` is nothing.
- The row the list prints is the word the run takes: `tools::skill::row` now carries `id`,
  and it is the same function the tool's own table prints, so the two faces cannot drift.
- An `id` picks a root. `skill --name x` refuses when two roots spell `x` (deliberately,
  and it stays that way); a path never has that problem, which is why `run_skill` takes one.

## `run_skill` starts nothing itself

`run_skill(id, cmd, session)` reads `cmd` with the `parse_cmd` that `subscribe` already
uses -- `{"name": …, "args": {…}}`, or a bare name -- resolves the id against the same
walk `ls_skill` makes, and then hands the `skill` tool the words `[name, --key value, …]`
through `tool_cmd::dispatch_tool`. Import, `GONGGA_SKILL_LIB`, the interpreter, the exit
code, the output cap and the body-with-no-entry refusal are therefore decided in one place.
A skill that is a body or a script has no functions to choose between, so `run_skill`
refuses it and names `skill` instead of guessing.

The tool learned one argument for this, `id`, which is the command layer's and never the
model's: it is not in `[tool_text.skill.arguments]`, so the catalog does not offer it and
the tool's own sentence still begins at `name`. Putting it in the content would be a
protocol bump for a word no model should say.

## The package is a uv project now

`python/pyproject.toml` names the same two modules the server embeds with `include_str!`
and writes into `<home>/lib`, so `uv run --project python` is a home without a home, and
"shipped uv python package" is the files that already ship rather than a copy of them.
`uv build`/`uv run` installs it; nothing about a home changes.

`gongga.py` also learned the one conversion the SPEC's `cmd: Cmd` shape needs: every
argument is a string on the wire (`protocol::Args`), so `call` sends a non-string as JSON
(`_wire`). `server.run_skill(id=…, cmd={"name": "add", "args": {"left": 2}}, session=…)`
now works as written, and a string still travels unchanged.

## Measured

One server, one temporary home, a package skill `harness/probe-pkg` (two functions) and a
body `harness/probe-body` (`.dev/probe_skill_cmd.sh`):

| call | answer |
| --- | --- |
| `ls_skill --session a` | 22 rows, `project/evolve … general/windows-dev` |
| `ls_skill --path harness` | `9 rows` |
| `ls_skill --path harness/probe-pkg` | `harness/probe-pkg` |
| `ls_skill --path har` | `no skill matches "har"` |
| `run_skill harness/probe-pkg {"name":"add",…}` | `5` |
| `run_skill harness/probe-pkg {"name":"shout",…}` | `HI` |
| `run_skill harness/probe-pkg {"name":"nosuch"}` | `probe-pkg has no function 'nosuch'; it publishes: add, shout` |
| `run_skill harness/probe-body …` | `not a Python package with functions: skill --name probe-body …` |
| `run_skill harness/nothing …` | `no skill is "harness/nothing" here; ls_skill lists …` |
| `lib/gongga.py` `run_skill(cmd={…})` | `5`, and `ls_skill` rows carry `id` |

`cargo fmt --check`, `cargo build` and `comments-check` (per file: `skill.rs` and
`commands.rs` both clean; the whole-tree run still has the six it had, in files this pass
did not touch).

## What this found on the way

A new server on this machine could not start at all: `no commit on main speaks protocol
9`. It was not the network and not the published content -- `ship/`'s `local` branch was
at protocol 9 and its `main` at 8, and a home's copy inherits both. `Fork::content` reads
`local` first, so a home made from that checkout was protocol 8. `git -C ship branch -f
main origin/main` fixed the checkout; a home copied from `.dev/ship9` needs the same one
line (`git branch -f local main`), which the probe now does.

## Left alone

- `skill`'s content prose does not mention `id`, on purpose (above). Its CLI spelling
  stays `--name`, and `-cli` refuses `--id` because the catalog does not offer it.
- Q5, the tool-forwarded commands' own vocabulary (`new`, `check`, `wait`, `bash`), is
  still the separate deletion.
- `run_skill` passes each `args` entry as `--key value`, which is what `gongga_skill`
  binds by name. A dict or a list value arrives as JSON text, not as a Python object: the
  contract has no field types beyond `int`, `float` and `bool`.
