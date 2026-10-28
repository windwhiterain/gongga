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

## LlmConfig::provider

Roster id of the provider this configuration points at, when it points at one.
`None` means the built-in default was used because the machine has no roster,
and it is only used in messages.

## LlmConfig::context

Context window of `model`, when something told us. `--compact_threshold` is a
percentage of this, so without it a percentage has no meaning.

## LlmConfig::request_timeout

Without it a provider that accepts the connection and then goes quiet parks the
turn forever: nothing in the process is waiting on a socket that ever finishes,
and no operator is watching to notice.

## LlmConfig::idle_timeout

Shorter than `request_timeout` on purpose: a response that is still arriving is
fine, one that stalled is not.

## LlmConfig::max_retries

Extra attempts after a request fails, times out, or comes back with no content at
all. `0` means one attempt and then the turn fails.

## LlmConfig::retry_base

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

Fold the older transcript into a summary once a request would carry more than
this many input tokens. `0` disables compression, which will eventually hit the
provider's context limit.

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

The roster (`~/.agents/llm.toml`) is the machine's own list of providers,
endpoints and model context windows. Consulting it means the common case needs
no configuration at all, and it is the same list `--llm` accepts, so there is one
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

## pick_entry

Precedence is "the more specific word wins": a provider and a model name an exact
entry; a model alone finds the provider that carries it; nothing at all takes the
roster's first usable entry.

A configuration that names something the roster cannot place is a warning rather
than a failure -- the built-in default still gets the server up, and the line
says what was actually used, which is how a typo is found.

## LlmConfig::builtin_tools / LlmConfig::modalities

Both come from the roster entry the session is on, never from this file. They are
capabilities of a *model*, not of the server: a session pointed at another
`--llm` must not inherit the default entry's web tools or its eyes, which is why
`for_entry` copies them along with the base URL and the model name.

`accepts_image` and `image_refusal` are the one reader of `modalities`, and
`image_refusal` quotes the list back, so the refusal names the roster entry that
said no. Empty is unstated and refuses nothing -- see
`docs/notes/server-providers.md`.

`write_default_if_absent` teaches both fields in the template's comment, because
that comment is where a reader learns there is a roster field at all. The file it
writes is the shipped one with this machine's values in it, so a home begins with
every prompt visible and editable rather than with a pointer into the binary.

## LlmConfig::for_entry

A session that names its own `--llm` is choosing a different brain, not a
different protocol: timeouts and retry policy stay exactly as configured.
The prose is not part of this struct any more at all -- a prompt belongs to
the request and is rendered per step, so a model swap cannot carry an
instruction from the model it replaced.

## toml_path

Windows separators are normalised to `/`: TOML would read `\` as an escape, and
every Windows API accepts forward slashes anyway.

## tests::the_instructions_are_well_formed

The text was assembled by line continuation in Rust until it moved into
`default.config.toml`, and the test stayed for the same reason: a stray escape
or a missing space is invisible until a model reads the prompt as one run-on
line. It now checks the shipped template, since that is what a home with no
opinion about prompts actually sends.

## tests::the_roster_answers_what_the_config_does_not_say

The roster is what makes "no `[llm]` section" work. These are the four ways a
configuration can name something, and the one way it can be wrong: a typo must
not stop the server from starting.

## persist_key, read_key_file

One line, the key, nothing else: the file is read with a trim, so an editor's
trailing newline is not part of the secret. An absent file and an empty file mean
the same thing -- "not here" -- so a half-written key file falls through to the
next source instead of authenticating with an empty string, which would be the
same hole as having no key at all.

## write_default_if_absent

"A file that exists is left alone" used to need a qualifier: the first start
minted a key into config.toml, so by the time the template writer ran the file
already existed, and the one document that explains the fields never reached the
disk. The exception judged a file as untouched when it held only that key. With
the key next door there is nothing left to excuse: absent means write, and
anything else is the reader's.

## tests::a_key_left_in_the_config_still_works_and_buys_its_file_quietly

The old place is not a migration project: a home that has the key in its config
resolves it, is not rewritten, and is not handed a second key file it will never
use. Two answers to "where is the key" are worse than the old one.

## tests::the_template_never_rewrites_a_file_someone_edited

One field is enough to count as edited. A configuration that was written by hand
is not ours to replace with a commented example.
