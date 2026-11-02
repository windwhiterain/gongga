# server/providers.rs

## providers

The machine's LLM roster: one `providers.toml`, from the community checkout.

One file already describes every provider a group of machines can reach: endpoints,
keys, model catalogue, context windows. Reading it is what lets a session say
`--model deepseek-official/deepseek-flash` and be understood, instead of every
caller restating a base URL and the name of an environment variable.

## Price

The roster records what each model charges -- `[provider.model.price]`, in whatever
currency and unit that provider quotes. This module reads exactly two of those numbers,
`input` and `cache_read`, and exposes one derived fact: `miss_ratio()`, how many fresh
tokens one cached token stands for.

Only the ratio is used, and deliberately: converting a currency or normalising a unit
would need a table of every provider's convention, and it would buy nothing. The plan
that consumes it compares sums of its own, so `1.00/0.10 CNY` and `0.15/0.015 USD` are
the same 10 and neither needs to know what a yuan is. `output` is read but unused --
nothing here plans against what an answer costs.

A model whose price the roster omits keeps `None`, and the cache's plan substitutes its
own stand-in rather than the roster inventing a number it was not told.

The file carries a lot this server has no use for -- quota endpoints,
holiday calendars. Only what a request needs is parsed and the rest is ignored,
so the roster can keep growing without breaking this.

Nothing here is fatal. A missing roster is a server that falls back to its own
defaults; a broken one is the same, plus a line saying why. Refusing to start
because a file we merely *prefer* is malformed would make the roster a worse
dependency than not having it.

## cache_ttl_secs

How long a provider holds a cached prefix is that provider's fact, so the roster states
it per `[[provider]]`, in seconds, and `Entry` carries it to the plan: a chain whose front
is older than that is dropped before the next plan chooses. The roster writes it in two
layers on purpose -- `cache_ttl` and `cache_ttl_estimate` are prose for a person ("5
分钟，命中即续期", "约 5 分钟"), and `cache_ttl_secs` is the number a program reads.
Nothing here parses prose, so a provider that states no number keeps `None`, and the plan
treats it as holding its cache forever.

A provider with more than one tier keeps its other tiers in `cache_ttl_tiers`, and
`cache_ttl_tier` names the one `cache_ttl_secs` is. Neither is read here: this server
sends no cache directive, so the tier in force is always that one, and the others are a
fact for whoever later teaches it to ask. See `docs/notes/server-prefix-cache.md`.

See `docs/notes/server-prefix-cache.md` for what the plan does with it.

## roster_paths

One file: `~/.agents/gongga-agents/providers.toml`, the community repository's
checkout. `GONGGA_PROVIDERS_CATALOG` still names a file and means it — an
override is a test rig or a fork.

**This used to be two files, merged field by field**, and the reason it is not is
the one the merge could never answer: a machine's own facts (its keys, its private
gateway, its self-hosted llama.cpp) had no home of their own, so they lived in an
overlay that had to be re-merged on every read, and the overlay and the checkout
disagreed about what "the current price" was. The machine's facts are now a branch
of the checkout: `local` on top of `main`, upstream changes arriving as a rebase.
A rebase is a conflict a person resolves once; a merge on every read is a
disagreement nobody looks at.

It also decides what can be committed. The repository is public, so it never
holds a secret: `key_env` names the variable, and the value lives in the
environment. A machine that keeps one on disk keeps it on its own branch, off the
remote.

## Catalog::load_file

One file, read once. A missing roster is the normal case for a fresh machine and
is not worth a line; a file that does not parse is reported once and costs the
whole roster, because there is no second layer to fall back on — the roster the
machine runs on is the only one there is.

Nothing but what a request needs is parsed. `[calendar]`, `[peak_off_peak]` and
the per-model fields this server ignores (`tier`, `params`) are carried through
untouched: the roster is also read by tools that want them, and a parser that
dropped what it did not understand would make the file un-annotatable.

## QuotaQuery, resolve_scripts

`[[provider.quota_query]]` is how this machine reads what a provider still
allows: `unit` says what the numbers mean (`percent-left`, `cny-left`, ...), and
`script` names the reader that fetches them. The roster's other fields there
(`url`, `jsonpath`, `auth`, ...) describe the endpoint for a human; nothing in
this crate speaks them.

`script` is a path inside the roster that wrote it, and it survives as the
absolute path it names: `resolve_scripts` rewrites each relative `script` while
the directory it came from is still known, because a path relative to a file is
only meaningful next to that file. Only a query that names a script is kept as
something the server can read.

## Catalog::quota_of

A provider's quota queries in file order, plus the credential a request to that
provider would use -- the scripts read the key from the environment variable
their own entry names, and passing the resolved key in is what makes a literal
key in the roster work as well as an exported one.

## One file, no merge

There is no `merge_into` any more, and the paragraph that used to sit here
described why not to replace a provider whole. Both were answers to a question
that no longer exists: with one file there is nothing to merge, and a machine's
disagreement is a commit on its own branch rather than an overlay.

File order still means what it meant — it is the default preference order
(`Catalog::default_entry`) — and `[model] provider` in `config.toml` is still
how a server's own choice is stated.

## Model::context

Input+output ceiling, when the provider publishes one. This is what a
`--context_limit` percentage is a percentage *of*.

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

One model as a caller picks it: which provider serves it, what name the API is asked
for, and where to ask. Nothing about whose key pays for it.

`model` is the pair and `model_name` is the half the API takes: the first is what a
session records and what a title shows, the second is what goes into a request's
`model` field. Two fields rather than one string split at the first slash on every
use, because the split has exactly one right answer and it belongs where the roster
is read. `model_title` is the roster's human name for it.

Kept separate from `Choice` because the two callers need different things.
`--model` must be able to say "this is wrong" at the moment it is typed, so it
wants a key. Startup must not: the server's own key comes from a longer chain
(an exported `GONGGA_API_KEY`, its `key_env` variable, or `keys.toml`), and
refusing to boot because `providers.toml` did not hold one would be a step
backwards.

## Model::modalities

What the roster says this model takes in and gives back: `["text", "image"]`,
`["text"]`, sometimes `["text", "image", "video", "audio"]`.

Treated as the whole of the image question. A model whose list is present and
lacks `image` is refused an image **by us**, naming the roster entry and the
missing modality -- the alternative is sending it and getting an opaque 400 from
a provider that never says which field it disliked.

Empty is not "nothing": unstated means the roster has no opinion, and no opinion
must not become a refusal. Roster files in the wild often omit the field.

## Model::builtin_tools

Tools the provider runs on its own side, in **the provider's own spelling**:
`web_search`, `web_search_preview`, a provider's own `web_fetch`, and so on.

Sent verbatim, with no mapping table. The names differ per provider because what
providers offer genuinely differs, and a table here would encode a vocabulary
this server cannot keep current. The roster is the right place because it is
edited by whoever knows the provider.

Absent means none are offered, and that default is load-bearing -- but not for
the reason first written here. A provider that does not recognise a tool may
fail every request, and may equally **silently ignore it**:

| Declared | Provider | What happens |
|---|---|---|
| `web_search` | `deepseek-v4-pro` | real searches; answers with `web_search_call` items |
| `web_search` | `deepseek-flash` | ignored, HTTP 200, no `*_call` item, model says it has no web access |

The second row is the dangerous one: nothing errors, the transcript looks
healthy, and the model simply answers from memory. So declaring a capability is
a claim that must be verified against the model it is declared for, not against
the provider. A roster entry asserting what a model cannot do is worse than an
absent entry, because it produces confidence instead of an error.

DeepSeek's own Responses guide states the general rule: unsupported parameters
are silently ignored so that existing clients can connect unmodified. That is a
compatibility feature for them and a trap for a roster.

## Entry::key

The key as written in the file, when it was written there. `None` is normal: a
roster that names a variable does not have to carry a secret.

## Entry::reasoning_levels

What the roster says the model's reasoning levels are, in the roster's own
spelling. Copied onto the entry so a command can validate a level without going
back to the model list.

## Entry::level_in_force

The level a session runs at when the command naming it does not say. Two answers, in
this order: the level the session already had, when this model still publishes it, else
this model's `reasoning_default`. Both are filtered through `reasoning_levels`, so the
result is only ever a level a request can carry -- a model that publishes no level
answers `None` even when it states a default, because `reasoning_default = "off"` (as
MiniMax-M3 does) is a mode rather than a level.

Called from `set_session`, which is what makes the stored level and the displayed one
the same value. See `docs/notes/server-commands.md`.

## Entry::reasoning_effort

Resolved at `set_session` time rather than per request: a level the model does
not take is a typo in the command that made it, and the roster can say what it
does take while the caller is still looking at what they typed.

## Catalog::problem

Why a layer could not be read, when one could not be. Kept rather than returned
so startup can proceed and still report it once, and kept as the *first*
complaint: with two layers, the one that failed parsing is the one worth naming,
and a list of three errors on one line reads as a broken roster rather than a
broken file.

## Catalog::models

Every model a request could actually be sent on here, spelled `provider/model_name`
in file order. This is the completion list for `--model`, so it promises only what
will run: a provider whose credential resolves takes part, and one that names a key
this machine does not have is left out.

The opposite choice -- offering the model and reporting the missing variable when it
is picked -- was tried and rejected: it makes Tab a list of what the roster knows
rather than a list of what this machine can do, and the failure it produces is one a
draw across providers cannot see coming. [[Catalog::resolve]] stays the authority on
whether a named model can run; `models` is only its answer, published early.

## Catalog::resolve

Resolves a model into a destination plus the key to use for it. This is what
`--model` is checked against: if it resolves, the next turn has somewhere to go and
something to authenticate with.

## Catalog::entry_by_name

A bare model name, searched in every provider in file order. It is what makes
`--model deepseek-flash` work when only one provider carries that name, and what
startup uses to turn `model_name = "deepseek-flash"` into a provider.
`resolve_by_name` is the same search for a caller that wants the key too.

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

## parse_value

Takes the merged tree rather than text, because merging happens on TOML values:
a layer can only override a field it can see, and the parse is what assigns the
merged result its meaning. Duplicate ids would make `--model` ambiguous, and the
resolution above silently takes the first. Keep the first and drop the rest
rather than pretend both exist.

## roster, Live, read_fresh

Read once per file, not once per process: an edit to either `providers.toml` reaches
the next `set_session --model` and the next `get_messages` completion list without a
restart, which is the whole reason a roster is a file rather than a build-time
table. The stamp is taken per file, because the files change for different
reasons: the community one when the clone is refreshed, the private one when a
reader edits it, and a server must not re-read 65KB for either.

That per-file question is also what lets the community layer arrive late. A
clone that lands 30 seconds after startup is not a restart: the next read sees a
file where there was none and merges it in.

The handle is an `Arc`, never a `&'static`. A `&'static` was honest while the file
was read once; keeping it through a reload would mean the retired list stays
alive in whatever holds it, which is worse than a copy: the server would resolve
against a provider the user has already deleted.

Each read is numbered, and `Agent::client_for` keys its cached clients by that
number too. A cached client is the one thing a reload must invalidate, because it
carries the base URL, the key and the model of the entry it was built from: reuse
one after the roster moved and the request goes to the old endpoint with the new
model name, which fails in a way no log line near the turn explains.

A turn still finishes on the client it started with. That is not the reload
failing; it is the same rule the timeouts follow, and the reason a long answer
does not change providers mid-sentence.

## roster

One file, one read: the command catalog asks for it to build completions on
every connect, and the agent asks for it on every turn that names a provider.

## tier

`tier` is carried as written -- `flash`, `pro`, `sota`, `test`, whatever a roster
author typed -- and never mapped onto a ranking of the server's. It is a judgement
about where a model belongs, so a server that re-derived it would disagree with the
file it just read. Nothing but SPEC's monitor reads it today (see
`server-monitor.md`); the rest of the roster still ignores it.

`Catalog::speaks_responses` is the same test `default_entry` applies, asked about
one named model. A caller choosing between models needs it: the roster knowing a
model is not the same as this server being able to drive it.
