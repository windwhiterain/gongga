# server/providers.rs

## providers

The machine's LLM roster: `~/.agents/llm.toml`.

One file already describes every provider this machine can reach: endpoints,
keys, model catalogue, context windows. Reading it is what lets a session say
`--llm deepseek-official/deepseek-flash` and be understood, instead of every
caller restating a base URL and the name of an environment variable.

The file carries a lot this server has no use for -- prices, quota endpoints,
holiday calendars. Only what a request needs is parsed and the rest is ignored,
so the roster can keep growing without breaking this.

Nothing here is fatal. A missing roster is a server that falls back to its own
defaults; a broken one is the same, plus a line saying why. Refusing to start
because a file we merely *prefer* is malformed would make the roster a worse
dependency than not having it.

## DEFAULT_PATH

Where the roster lives, relative to the home directory. DSH reads the same
path, which is the point: one file describes this machine.

## Model::context

Input+output ceiling, when the provider publishes one. This is what a
`--compact_threshold` percentage is a percentage *of*.

## Model::endpoints

Paths this model is known to accept. Empty means "not stated", which is treated
as "assume the provider's OpenAI endpoint works" -- the roster predates this
server and most entries never filled the field in.

## Model::reasoning_levels

Reasoning levels this model accepts, as the roster spells them (`low`, `xhigh`,
`on`/`off`, ...). Empty means "not stated", and a level is then passed through
untranslated: letting the provider answer for itself is better than this server
inventing a mapping.

## Entry

One resolved entry: everything a request needs, and nothing else. Where a
request goes: everything `--llm` names, and nothing about whose key pays for it.

Kept separate from `Choice` because the two callers need different things.
`--llm` must be able to say "this is wrong" at the moment it is typed, so it
wants a key. Startup must not: the server's own key comes from a longer chain
(an exported `GONGGA_API_KEY`, the DSH credential store), and refusing to boot
because `llm.toml` did not hold one would be a step backwards.

## Entry::key

The key as written in the file, when it was written there. `None` is normal: a
roster that names a variable does not have to carry a secret.

## Entry::reasoning_levels

What the roster says the model's reasoning levels are, in the roster's own
spelling. Copied onto the entry so a command can validate a level without going
back to the model list.

## Entry::reasoning_effort

Resolved at `set_session` time rather than per request: a level the model does
not take is a typo in the command that made it, and the roster can say what it
does take while the caller is still looking at what they typed.

## Catalog::problem

Why the file could not be read, when it could not be. Kept rather than returned
so startup can proceed and still report it once.

## Catalog::specs

Every `provider/model` worth offering a caller, in file order. This is the
completion list for `--llm`, so it includes entries whose key does not resolve
yet: choosing one and then being told to set a variable is a better path than
the model never appearing at all.

## Catalog::resolve

Resolves `provider/model` into a destination plus the key to use for it. This is
what `--llm` is checked against: if it resolves, the next turn has somewhere to
go and something to authenticate with.

## Catalog::find_model_entry

The same search as `find_model`, without requiring a key: what startup needs to
turn `model = "deepseek-flash"` into a provider.

## Catalog::first_entry_of

The first model of a provider, for `provider` configured without a model: naming
only the company still has to pick something.

## Catalog::default_entry

The entry a server with nothing configured should use: the first one that has an
OpenAI-compatible endpoint and can speak Responses.

File order is the preference order -- whoever keeps this roster puts the
provider they would reach for first at the top -- so this only skips what cannot
work rather than re-ranking what is left.

## openai_endpoint

`openai-responses` is preferred when a provider lists both flavours, because it
is the one this server actually speaks.

## credential

The environment wins over the literal: a file that names both is a file waiting
for the literal to be rotated out, and an exported variable is the operator's
most recent word on the subject.

## tokens_for_percent

This is exact arithmetic on a number the roster states: no character-to-token
guesswork belongs here. Converting the transcript into tokens is the agent's
job, and it does that by measurement rather than by a constant (see `Meter`).

## FileModel::reasoning_levels

Levels this model accepts, as the roster spells them. Empty means the roster
does not say, which is the common case for anything older than the field.

## parse

Duplicate ids would make `--llm` ambiguous, and the resolution above silently
takes the first. Keep the first and drop the rest rather than pretend both
exist.

## roster

One file, one read: the command catalog asks for it to build completions on
every connect, and the agent asks for it on every turn that names a provider.

## tests::a_percentage_becomes_a_token_budget

Never zero for a real percentage: zero reads as "compression is off" in the
agent, which is the opposite of what was asked.

## tests::reasoning_levels_are_read_and_mapped

The roster's reasoning fields are what `--thinking-effort` is checked against,
and the mapping is what turns a level a caller knows (`medium`) into one the
model actually takes (`high`).

## tests::a_level_is_mapped_then_checked

A level is mapped first, then checked: what the model is told is always one of
the levels the roster says it takes, and a level it does not take is refused
with the ones it does.

A roster that says nothing about levels cannot refuse one: the provider is the
only party that knows, so the level goes through.
