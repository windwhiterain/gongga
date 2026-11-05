# `crates/server/src/tools/skill.rs`

## module doc

The server already knew which skills exist: a separate tool read the three roots and published
rows with a name, a path and the entry. What a model could not do was *run* one -- and that
tool has since been folded in here: one registration answers the table when no `name` is
given and runs the entry when one is. Its only
route was `bash "…/web-search.sh fetch <url>"` -- a shell line it had to quote, from a path it
had to copy out of a body, on an interpreter it had to infer from an extension. The entry
runner already existed on the command line (`gongga-cli skill`), but a tool call cannot reach
a command layer's local route.

So `skill` is a tool, and because `tool_cmd` publishes every tool as a command, the operator
reaches it too. That is the whole reason it is a *tool* rather than a command: one
registration is both surfaces, and the model's call and the operator's line run one body.

## Why not `exec`

`exec` starts a program the caller names. A skill's entry is a program the *skill* names, and
which program starts it depends on a file this server can read and the model cannot be
expected to inspect: `.sh` needs a bash, `.py` needs a Python, `.ps1` needs PowerShell, a
`.cmd` needs `cmd /c`, and an extension-less entry is whatever its `#!` says. Making the model
spell that is making it re-derive `crates/skill/src/entry.rs` in prose, per call, in a machine
where `.sh` is not executable at all. `exec` stays for the words the caller owns; `skill` is
for the entry the skill owns.

The argv is a list, like `exec`'s, so the same property holds in both: no shell reads the
words. The command form takes it as JSON (`skill main web-search '["fetch", "<url>"]'`),
coerced by the tool's own schema, which is how an operator reaches the shape the model sends.

## The roots decision

The lookup is `installed_for` and nothing else: the same rows the table prints are the rows
it can run. A name in more than one root is refused with both directories
rather than resolved to the first -- `Roots` documents that a shadowed name is never resolved
silently, and running a file is the last place to break that.

A skill that is a body with no entry is a refusal, not an empty run: the answer is its
`SKILL.md`, and the message says so with the path to read.

## Where it runs

On this machine, in the session's own directory, whatever machine the session works on. The
entry is a file this server can open -- the checkout of a peer session is not in the roots at
all, so a `remote` session runs the home's and the machine's skills and is never handed a
path it cannot read. Its arguments are the caller's, and a peer path among them is the
caller's business.

## Output

What the entry printed, then `[exit status N]`, and `ok` is `code == 0` -- the entry's answer
is the call's answer, unparsed. Re-encoding it (into JSON, into rows) would be a second copy
of a contract every one of these scripts already keeps, and `jq`-ability is why they print
what they print. The cap is the shared one: over `max_output_chars` the text is spilled to a
file and the answer is that file's name.

## The table, and why it is not a second walk

`installed_for` walks the three roots once -- the same call `@skill:` expansion uses -- and
the project layer is the session's checkout, which is why it takes a cwd and a machine: the
same server serves sessions in different checkouts, and a skill in one of them must not
appear in another's table. A session whose directory is on a peer gets the home's and the
machine's rows, plus one sentence saying that checkout's skills are that machine's files to
`ls` and `cat` over `remote_bash`.

The description is the whole argument for the table: a reader decides from that line whether
to open the body, and the body is what costs context. A row is `name`, `description`, `path`
and `bucket` -- the four things needed to decide *and* to open, since `path` is the
`SKILL.md` `@skill:` hands out.

The rows are ordered by the prefix-cache plan for the session's owner, so a table that has
not changed is not re-paid for by a new session in the same directory. That is an ordering
of the same rows, never a filter: a skill that is installed appears, whatever the plan says.

`skill ls` in the command layer prints those four columns, asking this tool when the shell
names a session (`GONGGA_SESSION`) and walking the roots itself when it does not: a shell
with no session has no checkout to ask about.
