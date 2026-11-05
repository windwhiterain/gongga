# `crates/server/src/tools/text.rs`

## One place, and no default anywhere else

Every sentence a tool hands a model — its `description`, and the sentence on each of
its arguments — lives in the home's `config.toml`, under `[tool_text]`. The binary
carries none of them, so there is no second copy to drift and no build between a
wording fix and the model that reads it.

```toml
[tool_text.remote_python]
description = '''`python` on the session's remote endpoint ...'''

[tool_text.remote_python.arguments]
code = '''Python source to execute in the named interpreter. ...'''
```

`[tool_text.general.arguments]` holds the tunnel's own four — `description`,
`max_output`, `timeout_ms`, `detached` — and a tool that words none of them for itself
takes the sentence from there. That is one entry in one table, not a second layer: the
tunnel's words are the same sentence for fifteen tools, and fifteen copies of it would
be fifteen places to mistype it.

The name is `tool_text` and not `tools`: `[tools.bash]` is that tool's runtime settings,
and a table of settings must not be read as a table of sentences.

## A config that words nothing is a server that does not start

`Texts::missing` walks the registry and asks for both halves — the tool's own sentence,
and one for every argument its schema declares. What is missing is named at startup and
the server refuses:

```
Error: .../config.toml words nothing about ["bash (what it is)", "bash.background",
"bash.command", ...]; a tool's sentences live only there (`[tool_text.<tool>]`)
```

Measured, on a home whose shipped config had the section removed: exit 2, that list.

The alternative — sending a schema with no sentences, or with half of them — is a model
guessing what an argument is for, which is the failure this whole module exists to stop.
A home that predates the section is not stranded: `ship::install` merges the shipped
config into the home's at every start, so the words arrive with the content.

## What a config cannot do

It cannot add, remove or retype an argument. `apply` writes a `description` into a
property the schema already declares and leaves every other key alone: an argument the
tool does not read is not something a sentence can create, and a schema that grew a name
would be a call the model can make and the tool cannot answer.

A name no tool of that name declares is ignored, and said once at startup rather than
once per step — `definitions` asks for every tool on every step of every turn.

## Read after the shape, and re-read on a stamp

`general_parameters` applies the sentences *after* the tunnel has declared `description`,
`max_output`, `timeout_ms` and `detached`: a sentence is written into a property that
must already exist. Applying them first left those four without words, which is exactly
the kind of half-schema the startup check is there to catch.

`entries` compares the file's size and modification time against the last read, the way
`prompts` does, so an edit lands on the next step instead of the next start. A file that
does not parse yields nothing, and nothing is a refusal, not a guess.

## Measured

The move was checked against the schema the model saw *before* it: every one of the 17
tools' descriptions, every argument's sentence, every `type` and every `required` list
was compared field by field between the recorded request and the one built from config.
Zero differences. Then a reworded entry in a running home's config reached the next
request without a restart.
