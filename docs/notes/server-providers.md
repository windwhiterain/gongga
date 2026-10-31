# server/providers.rs

## providers

The machine's LLM roster: the community `providers.toml`, with the user's own over it.

One file already describes every provider a group of machines can reach: endpoints,
keys, model catalogue, context windows. Reading it is what lets a session say
`--provider deepseek-official/deepseek-flash` and be understood, instead of every
caller restating a base URL and the name of an environment variable.

The file carries a lot this server has no use for -- prices, quota endpoints,
holiday calendars. Only what a request needs is parsed and the rest is ignored,
so the roster can keep growing without breaking this.

Nothing here is fatal. A missing roster is a server that falls back to its own
defaults; a broken one is the same, plus a line saying why. Refusing to start
because a file we merely *prefer* is malformed would make the roster a worse
dependency than not having it.

## roster_paths

Two files, one per layer: `~/.agents/gongga-agents/providers.toml` is what the
community repository clones down (`agents::ROSTER` under each root), and
`~/.agents/providers.toml` is the machine's own. `GONGGA_PROVIDERS_CATALOG` still names one
file and means it: an override is a test rig or a fork, not a third layer.

The split exists because the two kinds of statement have different owners. A
model's context window, its modalities, its price bands and the shape of its
endpoint are facts about the provider, the same on every machine, and they
change when the provider does. A key, a private gateway's base URL, a
self-hosted llama.cpp -- those are facts about *this* machine. Keeping them in
one file meant every machine's copy drifted on its own, and a corrected price
had to be found and applied 15 times.

It also decides what can be committed. The community file is public, so it
never holds a secret: `key_env` names the variable, the secret lives in the
environment or in the private file. That is why the private layer is read
*last* -- it can correct anything the community says, and only it can say
"this machine's key is sk-...".

## Catalog::load_layers

Merges the layers in order, so the last file read wins a disputed field and the
first file read provides everything nobody disputed. A missing layer is the
normal case (a fresh machine, a machine that never wrote a private roster) and
is not worth a line; a layer that does not parse costs only itself, because the
other one still answers `--provider` and a roster half the size beats no roster.

Merging stops at what a request needs. `[calendar]` and `[peak_off_peak]` are
merged too, but nothing here reads them: they are merged because they are in the
same file and a private layer that overrode one of them would otherwise be
silently ignored by the tools that do read them.

## QuotaQuery, resolve_scripts

`[[provider.quota_query]]` is how this machine reads what a provider still
allows: `unit` says what the numbers mean (`percent-left`, `cny-left`, ...), and
`script` names the reader that fetches them. The roster's other fields there
(`url`, `jsonpath`, `auth`, ...) describe the endpoint for a human; nothing in
this crate speaks them.

`script` is a path inside the layer that wrote it -- the AGENTS notes say a
`providers/x.py` written in one layer means that layer's copy -- and the merge
throws the layer away. So the path is resolved before `merge_into` sees the file:
`resolve_scripts` turns each relative `script` into the absolute path it names,
per file, while the file it came from is still known. Only a query that names a
script is kept as something the server can read.

## Catalog::quota_of

A provider's quota queries in file order, plus the credential a request to that
provider would use -- the scripts read the key from the environment variable
their own entry names, and passing the resolved key in is what makes a literal
key in the roster work as well as an exported one.

## merge_into

Field-level, keyed by `id` for arrays of tables: `[[provider]]` and, under it,
`[[provider.endpoint]]` and `[[provider.model]]`.

The alternative -- a private `[[provider]]` replacing the community one whole --
was rejected because of what the private file is *for*. Its most common content
is one field: `key = "sk-..."` for a provider the community already describes.
Whole-provider replacement would make that a 40-line copy of the community
entry, which then rots: the community fixes a price, the copy keeps the old one,
and nothing says which is right.

Arrays of tables pair up by `id` only when every element on both sides has one.
That rule keeps `modalities = ["text"]` overriding as a value -- a list of
strings is a value, and merging it element-wise would be inventing semantics the
roster never had -- while `[[provider.model]]` merges per model.

Order is the base's, with private-only entries appended. File order is the
default preference order (`Catalog::default_entry`), so a private layer can add
a provider at the end and cannot silently promote itself above the community's
first choice; making it the default is what `[llm] provider` in `config.toml`
is for.

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

One resolved entry: everything a request needs, and nothing else. Where a
request goes: everything `--provider` names, and nothing about whose key pays for it.

Kept separate from `Choice` because the two callers need different things.
`--provider` must be able to say "this is wrong" at the moment it is typed, so it
wants a key. Startup must not: the server's own key comes from a longer chain
(an exported `GONGGA_API_KEY`, its `key_env` variable, or the roster's `key`),
and refusing to boot because `providers.toml` did not hold one would be a step
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

## Catalog::specs

Every `provider/model` worth offering a caller, in file order. This is the
completion list for `--provider`, so it includes entries whose key does not resolve
yet: choosing one and then being told to set a variable is a better path than
the model never appearing at all.

## Catalog::resolve

Resolves `provider/model` into a destination plus the key to use for it. This is
what `--provider` is checked against: if it resolves, the next turn has somewhere to
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

## parse_value

Takes the merged tree rather than text, because merging happens on TOML values:
a layer can only override a field it can see, and the parse is what assigns the
merged result its meaning. Duplicate ids would make `--provider` ambiguous, and the
resolution above silently takes the first. Keep the first and drop the rest
rather than pretend both exist.

## roster, Live, read_fresh

Read once per file, not once per process: an edit to either `providers.toml` reaches
the next `set_session --provider` and the next `get_messages` completion list without a
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

## tests::modalities_and_builtin_tools_reach_the_entry

Both fields are read off the roster and carried on the entry, and a model that
declares neither ends up with empty lists rather than with defaults: the server
must not offer a tool, or refuse an image, on a model whose roster entry said
nothing about either.
