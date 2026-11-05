# `crates/server/src/tools/text.rs`

## What lives here

The sentences a tool hands a model: its `description`, and the sentence on each of its
arguments. Both are what a config may reword; nothing else about a tool is.

`[tool_text.<tool>]` in the home's config carries `description`, and
`[tool_text.<tool>.arguments]` carries one line per argument name:

```toml
[tool_text.remote_python]
description = '''`python` on the session's remote endpoint ...'''

[tool_text.remote_python.arguments]
code = '''Python source to execute in the named interpreter. ...'''
```

The name is `tool_text` and not `tools`: `[tools.bash]` is that tool's runtime settings,
and a table of settings must not be read as a table of sentences. A section shared by
both would mean a config edit about a timeout could reword a prompt.

## Three layers, merged per field

pinned (the ship content's `config.toml`) < the home's `config.toml` < the code's own
default.

The code's default is the bottom because it is the one a home may never mention: a
tool added tomorrow says something even in a home whose config predates it. The pinned
content is where a wording that should reach everybody by default belongs. The merge is
per field, not per tool, so a home that overrides one argument keeps the shipped
sentence on the others.

Measured on a real server (`scripts/fake_llm.py` and `.dev/verify/probe-remote-tools.sh`):
appending the two lines above to a running home's `config.toml` and sending a second
message made the next request carry exactly those words, with no build in between.

## What a config cannot do

It cannot add, remove or retype an argument. `apply` writes `description` into a
property the schema already declared and leaves every other key alone: an argument the
tool does not read is not something a sentence can create, and a schema that grew a
name would be a call the model can make and the tool cannot answer.

A name the tool does not declare is ignored and warned about once, through `warn_once`
-- once per process, not once per step, because `definitions` asks for every tool on
every step of every turn.

## Re-read on a stamp, like the prompts

`entries` compares the file's size and modification time against the last read, the
same way `prompts` does, so an edit lands on the next step instead of the next start.
A file that does not parse yields nothing and the layers below stand: a typo in a
sentence must not be able to stop a server, which is why this is a `# Safety` note in
the code and not a refusal.

## Why it is config at all

These sentences were edited in Rust every time a model was misunderstood, and every
edit was a build, a restart and a new distribution on the peer. They are prose, and
prose is what `[prompts]` already keeps in config: the difference between the two is
that a prompt template is indexed by name and a tool's words hang off the schema the
tool itself declares.
