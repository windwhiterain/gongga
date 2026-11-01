# `crates/server/src/prompts.rs`

## module doc

Every word this server hands a model as prose -- the system instructions and the
few notes it injects around a transcript -- is a string template in a config file.
The alternative was the status quo: eight string literals spread over four modules,
each one reachable only by recompiling, and each edit invisible to whoever was
actually reading what the agent said.

Nothing here holds the prose. A home's `config.toml` is the file a reader edits, and
the pinned ship content behind it is what answers when the home says nothing; both are
read from disk. The one list this module does own is `REQUIRED`, because the code is
the only party that knows which names it asks for.

## SYSTEM: read, never rendered

`instructions` opens every request. A placeholder filled from the session would give
each session its own first bytes, and a provider's prefix cache is keyed by bytes: the
whole point of those bytes being first is that every session of a home shares them, so
the cache still holds the prose and the tool schemas and the start of the transcript
behind it. So this one name is `text()`, not `render()` -- `system()` is the accessor,
and a `{{session}}` written into it reaches the model as written.

The session name did not go away: every shell has `$GONGGA_SESSION`, which is where a
session that needs to know what it answers to looks. The immediate reason for the split
was that the prompt had grown into a page of procedures -- how to batch reads, how to
drive `gongga-cli`, which bucket a skill lives in -- and a page of prose in front of
every request is exactly what a skill is for. The identity, the standing authorisation
to run commands, and "procedures are skills" are what is left, because none of them is
a procedure and all of them are true in every turn.

Everything else stays a template: the notices are rendered per event, where the numbers
and the session and the spill path really are different every time.

## REQUIRED, TABLE

The check that replaces a compiled-in copy. Without it the failure is silent and
total: a home whose config has no `[prompts]` and whose `ship.git` cannot be read
would send an empty system prompt, run every tool, and look healthy in the log.
`main.rs` refuses to start on a non-empty answer, naming the templates and the pin.

`TABLE` is the one section read. The rest of the config is the loader's business, and
this module must not learn to parse `[provider]` -- two readers of one file disagreeing
about what a bad line means is worse than one of them being blind to it.

## Stamp, Stamp::of

The reload rule is a file size and a modification time, not a hash. A prompt edit is a
human typing into a file once, and the cost of missing it is one request with the old
wording; the cost of hashing the config before every model request is paid on every
request forever. The same argument is why `scan_fingerprint` in `script.rs` compares
mtimes.

`templates()` keeps one snapshot behind a mutex and compares the stamp first, so a
steady server re-reads nothing. The stamp is `Option`: a missing file is a state, not
an error, and "still missing" is a match.

## read_table

Returns the templates *and* the problems it found, and a file that does not parse
yields nothing at all -- the pinned content then answers, because the home's own last
good read is replaced by it. This is the same rule the config loader already applies to
a hand-edited `cli.toml`: a broken file is damage, not a reason to refuse to run, and
refusing would take the agent's tools away because of a comma.

A non-string value (`size_hint = 7`) is reported and skipped, not fatal: one mistaken
key should not delete the other eight templates the home did write.

## templates() -- the warnings

An unknown name is almost always a misspelling, and a misspelled template is invisible:
the server keeps running, the old wording keeps being sent, and nobody reads a log line
that says nothing happened. So the name is named, once per read, and only for a name
that is neither on `REQUIRED` nor in the pinned content.

`complained` is why the same damaged file does not produce one line per request.

## reference_index

The one template whose value is not a notice but a document: the text of every
reference the conversation named, each after the spelling that names it. It is rendered
once, into the first prompt row, and copied verbatim by every row after it -- growing
it would re-bill the transcript behind it. `{{entries}}` is the whole body, so the order
the entries arrive in is `prefix_cache`'s rather than the template's: a reader who
reorders that text is reordering a cache, not a document.

## substitute

For the notices only -- never for `SYSTEM`. `{{name}}` and nothing else: no
conditionals, no loops, no filters. A prompt that needs
a conditional is a prompt with two shapes, and two shapes are two templates named by the
code -- which is what `context_notice` does on its own. A real
template language would buy those two cases with a dependency and a second syntax to
learn about the harness.

## substitute -- the # Invariant

An unsupplied placeholder stays in the text. Deleting it would take the sentence with
it: a notice that lost its numbers reads as "you are near the limit", which is exactly
the message this project already rejected as unactionable. Leaving `{{session}}` visible
in what the model reads makes the bug obvious in the transcript, where a silently
shortened sentence never would be.

## install, shared

A process global, because the prose is wanted at layers that carry no config: `store.rs`
needs the interrupted-call line while building history, `llm.rs` needs the size hint
while a stream is failing. `install` takes the home's config, layers the pinned ship
content under it, and *returns what is missing*, which is the reason the global can be
trusted by the time the first request is built.

`shared()` before `install` is empty rather than fatal. A test that wants prose takes
`Prompts::of`, and a production caller reaches the global only after startup checked it.

## Prompts::of, and who takes a `&Prompts`

`cap_output`, `image_refusal`, `vision`, `with_size_hint`, `context_notice` all take
the templates as an argument. Not for tidiness: each of them
used to read prose off a compiled-in file inside a unit test, which stopped being
possible the moment the prose left the binary. A parameter keeps the test honest about
the mechanism it owns -- that the numbers arrive, that the spill path is named, that the
provider's own sentence survives -- while the words themselves are the config's to say.

## no rules file is read

The server reads no `AGENTS.md`. Two used to be joined into `instructions` when a prompt
row was written -- the machine's `~/.agents/agents.md`, then `AGENTS.md` beside the
session's directory -- and both reads are gone, along with the `constitution` template
that wrapped them. Standing rules now live where this machine keeps every other
procedure: a skill under `~/.agents/skills/`, listed by `index_skill`, read when a task
matches its description.

That is the same argument the skill root already makes, applied to the one prose that
ignored it. A rules file was in every request whether or not the turn needed it, and an
edit to it changed what the model was told without any row recording that the user said
it. A skill costs a name and a description until the work asks for the body.

What stays is where the reads sit: the templates are re-read whenever the file's stamp
moves, and `prompt_in_force` renders them at every step and stores a new row only when
the text differs. So a template edit is in the next request rather than the next turn,
and a step whose prose did not change writes nothing.

## where the prose comes from, and why not from here

An earlier shape of this module embedded `default.config.toml` with `include_str!` and
copied it into a new home. That put the same words in two places with different
lifetimes: the binary's copy froze at build time, the home's copy was the one anybody
read, and a fresh home inherited values computed by the loader rather than the file it
was handed. Both are gone. `ship::config_text` reads the pinned blob out of the home's
own `ship.git`, so the fallback moves when the pin moves and the home's file is the one
the server runs on -- `main.rs` re-reads it after the install for exactly that reason.
