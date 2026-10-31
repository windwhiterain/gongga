# server/config.rs

## DEFAULT_MAX_OUTPUT_CHARS

Tool output larger than this is capped and spilled to disk. Sized so an ordinary
command still comes back whole while a `cat` of a source file does not smuggle
the whole file into every later request.

## DEFAULT_COMPRESS_ABOVE_TOKENS

Compress once a single request would carry more than this many input tokens.
Only a fallback for a model whose context window the roster does not state;
otherwise the threshold is a share of the window itself.

## DEFAULT_KEEP_RECENT

Messages kept verbatim after a compression: enough that the model still has the
last few steps in full, not just a paraphrase of them.

## ModelConfig::provider

Roster id of the provider this configuration points at, when it points at one.
`None` means the built-in default was used because the machine has no roster,
and it is only used in messages.

## ModelConfig::model_name

The half of a model the API takes in its own `model` field. `ModelConfig::model` is
the whole thing -- `provider/model_name` when the provider is known, the bare name
when the roster could not place it -- and it is what a session records and a title
shows. The two exist because a request needs the second and every answer about "what
are you running" needs the first.

## ModelConfig::context

Context window of the model, when something told us. `--context_limit` is a
percentage of this, so without it a percentage has no meaning.

## ModelConfig::headers_timeout

The provider has this long to answer with response headers, the upload of a
request included. A provider that accepts the connection and then goes quiet
never sends them, and nothing else in the process is waiting to notice. It
bounds nothing about the body: see `idle_timeout`.

## ModelConfig::idle_timeout

The longest the model may go without *saying* something. Only an event that
carries content moves this clock -- a chunk holding nothing but keep-alive
noise does not -- so a provider that holds the socket open while it thinks is
as stalled as one that sends no bytes at all.

There is deliberately no wall-clock bound on the body. A total deadline covers
the response as well as the answer, and it cannot tell "not answering" from
"still writing": measured on real runs, one response reached 470 s (46.8k
output tokens), and every cut-off landed at the deadline to the millisecond
rather than at any network event. A response's length is bounded by the
provider's own output cap, not by a socket timer.

## ModelConfig::max_retries

Extra attempts after a request fails, times out, or comes back with no content
at all. `0` is no ceiling: retry until the turn is cancelled.

## ModelConfig::retry_base

First retry delay; every further attempt doubles it, capped at `MAX_RETRY_DELAY`,
so a provider having a bad minute is waited out rather than hammered.

## BashConfig::timeout

The number decides when the model stops waiting, nothing else.

## BashConfig::max_output_chars

Tool output longer than this is capped: only the head and the tail come back to
the model, the whole thing is written to disk. Without this a single `cat` of a
source file parks tens of kilobytes in the transcript and every later step pays
for it again.

## AgentConfig::compress_above_tokens

Send the context notice once a request would carry more than this many input
tokens. `0` disables the notice, which will eventually hit the provider's context
limit.

## Config::load_with_home

`--home` is the most specific thing the operator typed, and it also decides where
the config is looked for when `--config` is not given. Everything downstream --
database, logs, detached-job records, the instance lock -- already keys off this
one field, which is why there is nothing else to override.

## Config::load_with_home (key)

Without a key anyone on the network could drive this machine's shell, so an
unconfigured server mints one and writes it down rather than starting open.

It writes it to `server.key`, beside the config and not inside it. A config file
is content the harness ships, a reader edits, and the two merge; a minted secret
has no business in a document with two authors, because the merge would either
overwrite the secret or be blocked by it. A home that predates the split keeps
working: `[server] key` in its config.toml is still read, after the key file and
before a fresh mint, so no home is told its own clients are suddenly wrong.

## Config::load_with_home (roster)

The roster (`~/.agents/providers.toml`) is the machine's own list of providers,
endpoints and model context windows. Consulting it means the common case needs
no configuration at all, and it is the same list `--model` accepts, so there is one
answer to "what models exist".

## Config::load_with_home (api key order)

The roster is the machine's own answer, so a key written in it is used before
any fallback: a file that names the key is more specific than one that only
names a variable.

A provider key comes from one of three places, in order: `GONGGA_API_KEY`, the
variable that provider names (`api_key_env`), or the `key` written in the roster.
Nothing outside this repository is consulted.

## Config::load_with_home (max_retries)

`0` is no ceiling: retry until the turn is cancelled. A long run must not die
because a provider had a bad minute at hour three, and nothing about a
provider's recovery time is knowable in advance.

## pick_model

Precedence is "the more specific word wins": a model named in full carries its own
provider and is placed whatever else is said; otherwise a provider and a model name
are the two halves of one model; a model name alone finds the provider that carries
it; nothing at all takes the roster's first usable entry.

The first rule is what lets `model = "deepseek-official/deepseek-flash"` and
`model_name = "deepseek-flash"` be the same setting written two ways, instead of two
settings that could disagree.

A configuration that names something the roster cannot place is a warning rather
than a failure -- the built-in default still gets the server up, and the line
says what was actually used, which is how a typo is found.

## ModelConfig::builtin_tools / ModelConfig::modalities

Both come from the roster entry the session is on, never from this file. They are
capabilities of a *model*, not of the server: a session pointed at another
`--model` must not inherit the default entry's web tools or its eyes, which is why
`for_entry` copies them along with the base URL and the model name.

`accepts_image` and `image_refusal` are the one reader of `modalities`, and
`image_refusal` quotes the list back, so the refusal names the roster entry that
said no. Empty is unstated and refuses nothing -- see
`docs/notes/server-providers.md`.

Both fields are taught in the shipped config's comments, which is the file a home
actually receives -- see `docs/notes/server-ship.md`. Nothing here writes a config:
a value the loader invented would have to be copied into a file to be edited, and
the copy is how this project lost the argument about which of the two was true.

## ModelConfig::for_entry

A session that names its own `--model` is choosing a different brain, not a
different protocol: timeouts and retry policy stay exactly as configured.
The prose is not part of this struct any more at all -- a prompt belongs to
the request and is rendered per step, so a model swap cannot carry an
instruction from the model it replaced.

## prose this module no longer owns

Two tests used to read the system instructions out of a compiled-in file and
complain about a stray escape or a missing space. The prose left the binary, so
the tests left with it: a Rust test that asserts a sentence in another repository
is a build-order dependency disguised as a test. What is still checked in Rust is
the shape of the hand-over -- that `image_refusal` receives the roster's own
modality list and that an unstated list refuses nothing -- which is this module's
claim, not the config's wording.

## Config::config_path

`Config::config_path` is the file the loader actually read, kept on the struct so the
prompt templates are read from that same path. A server started with `--config
/tmp/other.toml` would otherwise take its values from that file and its prose from
`<home>/config.toml`, which is two answers to one question -- and the second one is
where a test home's edits would silently never land. It is made absolute for the same
reason `home` is: a relative path means something different to whatever child reads it
next.

## tests::the_roster_answers_what_the_config_does_not_say

The roster is what makes "no `[model]` section" work. These are the four ways a
configuration can name something, and the one way it can be wrong: a typo must
not stop the server from starting.

## persist_key, read_key_file

One line, the key, nothing else: the file is read with a trim, so an editor's
trailing newline is not part of the secret. An absent file and an empty file mean
the same thing -- "not here" -- so a half-written key file falls through to the
next source instead of authenticating with an empty string, which would be the
same hole as having no key at all.

## the section is `[model]`

The table says what a session runs on, so it is named after the thing it holds: the
`[model]` table, `ModelConfig`, and a `model_name` inside it for the half the API
takes. It was `[llm]`, then `[provider]`, and the second name was the mistake: a
provider is the company, not the brain, and a table called `[provider]` holding a
`model` was the same confusion one level down. Both older spellings are still read
-- `[llm]` fell back to the roster, `[provider]` is a serde alias -- so an old home
keeps working and the next install merges the new table in.

## who writes config.toml

Not this module. The install in `ship.rs` places the pinned file into a home that
has none and merges into one that does, so "absent means write, anything else is
the reader's" is enforced by the same rule that governs the skills. The historical
qualifier -- a first start minted a key into config.toml, so the file that had to
explain the fields always looked already-answered -- died with the key moving next
door to `server.key`.

## tests::a_key_left_in_the_config_still_works_and_buys_its_file_quietly

The old place is not a migration project: a home that has the key in its config
resolves it, is not rewritten, and is not handed a second key file it will never
use. Two answers to "where is the key" are worse than the old one.

## tests::a_key_left_in_the_config_still_works_and_buys_its_file_quietly

One field is enough to count as edited. A configuration that was written by hand
is not ours to replace with a commented example.
