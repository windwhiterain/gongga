# cli/commands.rs

## commands.rs

This file is the grammar of a command line and nothing else: `parse_args` binds
the tokens, `signature` spells a spec back, `roots`/`installed_skills` read where
a skill lives. The state these were once shared with -- sticky entries, drawn
sessions, block size, box edge, the remembered address -- is the window's, and
moved to `crates/tui/src/state.rs` (`docs/notes/tui-state.md`).

That move is also why a one-shot `gongga-cli` cannot inherit an operator's
session: the command layer reads no `client.toml` at all, so an argument it was not
given is an argument nobody gave it.

## EVENT_FIELDS

An `--name` that no event carries filters nothing, and silently doing nothing is
how a user concludes the log is broken. The list is the wire vocabulary, and
`set_event_filter` refuses anything outside it with the list in the message.

`session` and `sessions` both appear because both are on the wire: the first
tags a stream, the second is the session list event. They mean different things
on purpose; `session=main` does not hide the list.

## parse_args

One parser binds a line to the published spec: the line carries a cursor over
the declaration, and a bare word takes the first unfilled parameter from there.
`--flag value` / `--flag=value` name a parameter outright. Both spellings move
the cursor to one past the parameter they bound, so a flag advances it exactly
as a bare word does -- and naming a parameter behind the cursor pulls it back.
No parameter is ever bound twice, which is why a bare word steps over the ones
already on the line: `set_session main --model flash high` binds name by
position, model by flag, then thinking by the cursor the flag left.

Two consequences worth stating, because they are the rule and not an accident.
An optional parameter ahead of the cursor takes the word first: on
`--ctx_limit 30 high`, `high` is the model, since the cursor stopped at the
parameter after `--ctx_limit`. And a cursor past the last parameter wraps to the
first unfilled one, which is what keeps a flag that sits early from stranding the
required parameter behind it: `--must_create true main` still binds `main` to
name, and `--model flash main` binds it to thinking, then refuses the missing
name. The rule lives in `next_unfilled`, with `after_arg` for the cursor a flag
leaves, and the completion replays the same two steps (`bound_words`). The spec is the only syntax the client
knows -- the transparent wrapper keeps no command list of its own -- so a
`--word` that names nothing in it is a typo said as such, and a bare word with
no parameter left to guess is "takes <signature>".

Typed arguments win over sticky ones: the binding happens first, and the window
fills only slots still empty afterwards (`docs/notes/tui-run.md`). The pure-text
expansion names only the message (`send_message --message <text>`): the session is
the window's entry to fill, and a line that names neither is the server's refusal
to answer.

The flag value is whatever follows its flag, *including* a token that starts with
`--`. Refusing those made the one thing a message flag exists for impossible:
`--message "--version"` was read as a missing value, and the only way through
was `--message=--version`, which nothing advertises.

The cost is the other direction: a forgotten value swallows the next flag, which
then fails on whatever follows it -- `send_message --message --session hi` reads
`--session` as the value of `--message`, and `hi` is what fails.

## installed_skills

The three roots the server walks, in its order, from the one reading of them:
`skill::Roots` in `crates/skill`. Completion still runs in a key handler and cannot ask
the server, so both sides read the same directory walk rather than two walks that are
kept equal by hope -- which is what this used to be, and what a third caller
(`gongga-cli skill`) would have broken.

`project` is `None` when the session whose skills these are runs on another
machine. Its checkout is then not on this disk, and reading
`<that path>/.agents/skills` here would offer the local directory of the same
name -- a name that cannot be opened, which is the failure this whole area keeps
producing. The home's and the machine's still count: they are what the server's
own `@skill(...)` expansion reads, since that expansion runs on the server.
