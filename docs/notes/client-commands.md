# cli/commands.rs

## commands.rs

This file is the grammar of a command line and nothing else: `parse_args` binds
the tokens, `signature` spells a spec back. The state these were once shared with --
sticky entries, drawn
sessions, block size, box edge, the remembered address -- is the window's, and
lives in `crates/client/src/window_state.rs` (`docs/notes/tui-state.md`).

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

## skills: the server's list, not one of ours

`CompletionSource::Skills` is a live source the front-end fills in, and the walk is the
server's: `Client::ask_skills` sends `list_skills` for the session the line names
(nothing named is the server's own directory), `fill_skills` keeps the rows under that
session, and both the `@skill(...)` reference and `run_skill{"id":` read them -- with
the `SKILL.md` path as the popup's right-hand column.

The client used to walk the three roots itself, with `<home>/skills` in the middle.
That was the same walk exactly while the server copied the content into the home; it
does not any more, so the middle root is `<checkout>/<declared skills>`, which is a path
this layer does not own. What the walk offered was the last copy an older build made:
`heir`, retired by the content, and none of `skill`, `image`, `providers` or
`prefix-cache`, which no build had ever copied there. `docs/notes/client-catalog.md`
carries the rest.

A refusal is `fail_skills`, for the reason `fail_paths` has one: a question the
completion remembers as in flight is never asked again.

## sticky_kinds

One list of the types a sticky entry may name, and it is a list of what can actually
fill: a **required** argument's `kind` (`session`), a **structured value**'s own type
when the argument is not an update (`config`), and a field's type when the field has
no default (`model_config`, `model`). That is SPEC's four: `Session`, `Model`,
`ModelConfig`, `SessionConfig` -- `Workspace` is not marked, and a defaulted field is
not a missing one, so neither is in the list. `set_sticky` refuses what is not there,
`--type` completes what is, and only a listed type is written back.
