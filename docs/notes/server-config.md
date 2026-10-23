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

## Config::load_with_home (roster)

The roster (`~/.agents/llm.toml`) is the machine's own list of providers,
endpoints and model context windows. Consulting it means the common case needs
no configuration at all, and it is the same list `--llm` accepts, so there is one
answer to "what models exist".

## Config::load_with_home (api key order)

The roster is the machine's own answer, so it is consulted before the DSH
credential store rather than after: a file that names the key is more specific
than one that only names a variable.

Last resort: the credential store DSH keeps on this machine, so a provider key
does not have to be exported by hand.

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

## LlmConfig::for_entry

A session that names its own `--llm` is choosing a different brain, not a
different protocol: timeouts, retry policy and the shell instructions stay
exactly as configured.

## toml_path

Windows separators are normalised to `/`: TOML would read `\` as an escape, and
every Windows API accepts forward slashes anyway.

## dsh_credential

Only the flat `refs:` mapping is understood. The file is DSH's, not ours, so
anything it adds elsewhere is ignored rather than parsed.

## tests::the_instructions_are_well_formed

The instructions are assembled by line continuation, where a stray escape or a
missing space is invisible until a model reads the prompt as one run-on line.

## tests::continuation_lines_are_not_treated_as_keys

A folded value keeps its leading spaces, which is why the block test above
matters: only two-space keys belong to `refs`.

## tests::the_roster_answers_what_the_config_does_not_say

The roster is what makes "no `[llm]` section" work. These are the four ways a
configuration can name something, and the one way it can be wrong: a typo must
not stop the server from starting.
