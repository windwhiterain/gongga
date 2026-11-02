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

## SYSTEM: the prose, with the session's name filled

`instructions` opens every request, and `{{session}}` in it is the one placeholder
`system()` fills. It was `text()` rather than `render()` for a year of this design, for
a reason that turned out not to hold: "a placeholder filled from the session would give
each session its own first bytes, and the cache is keyed by bytes." The cache is keyed
per *conversation* -- `prefix_layout` is a row per session, and the note on caching says
so outright -- so no session's bytes were ever warm for another. Nothing was being
shared, and the cost of the name was zero.

What it did cost was the name. The prose said "Your session name is `$GONGGA_SESSION`",
which nothing substitutes, so a model read the literal and reported that its prompt did
not tell it its name -- measured in a session called `probe`, whose whole first turn was
that question and the tool call it needed to answer it. The variable is real in every
shell, and that is exactly the point: knowing the name should not need a shell.

A name is stable for a session, so the bytes stay stable for every request of one, which
is all a cached prefix needs. The card (`dynamic_tail`) still states it too, with the
build, the first minute and the directory.

The reason the prose was cut down to a page-or-less still stands: it had grown into
procedures -- how to batch reads, how to drive `gongga-cli`, which bucket a skill lives
in -- and a page of prose in front of every request is exactly what a skill is for. What
is left is identity, the standing authorisation to run commands, and "procedures are
skills": none of them is a procedure and all of them are true in every turn.

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

## REVISION

What introduces a patch for an entry of the index that has changed since it was sent:
`@@`-headed hunks, each under the spelling it corrects. `{{revisions}}` is the whole
body, in the order the entries are emitted.

Deliberately absent from `REQUIRED`. A home pinned before this template existed has an
index and no use for a patch, and refusing to start over prose it may not need is the
wrong trade; `Agent::index_tolerance` asks whether the template is there and stops
patching when it is not -- the same answer a tolerance of `0` gives.

## dynamic_tail

The session's start card: name, the harness and the commit it runs, the minute of the
**first** turn, and the directory that turn stood in. Stamped once into the first prompt
row and replayed verbatim from the store after that -- it is sent as the last input item,
and it never changes, because what a request has sent is never rewritten. `{{build}}` is `protocol::BUILD`, the same identity a bug
report carries, so a model reading its own prompt can say which build it is.

It is not part of `instructions` and not part of the transcript either: `build_head`
writes it as the request's first `input` item, which is where SPEC puts it. See
`docs/notes/server-body.md` for why both halves of that position matter. What it
buys: a session that is asked "what time is it" or "where are you" answers without a
tool call, and a transcript replayed later says when it happened and which build said it.

## dynamic_tail_remote

The same card when the directory is on a peer, and it says so: `{{cwd}}` alone does not
tell a model whether a path is this machine's or the remote's, and the wrong guess is a
`bash` call that fails on a path that exists -- somewhere else. Two templates rather
than one with a conditional, for two reasons: `substitute` has no conditionals by
design, and a session on this machine keeps exactly the bytes it already sent.

A third template, `dynamic_tail_remote_unstated`, is the peer with no directory known:
naming one is optional (`--cwd` belongs to the peer), and a session's first card is the
one it keeps -- `moved` deliberately does not rewrite a row whose recorded directory is
empty, because the shell reporting where it started is not a move. Without that shape
the default setup of a remote session would send "in ``" for the life of the session.

Which one is chosen is the machine the shells are on (`Prompt.remote`, read from
`Shells::remote_name`) and whether a directory is known, and the row records the machine.
That record is why `--remote none` in the directory the session left is a move:
`Prompt::rewritten_by` compares the texts, but the *choice* of template is decided by a
value that has to be recorded to be compared.

## substitute

For the notices, and for the one name in `SYSTEM`. `{{name}}` and nothing else: no
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

## one rules file, and it is a root

The server joins no `AGENTS.md` into `instructions`. Between 2026-10-28 and 2026-11-02 it
read none at all: `prompts::constitution` used to join two of them -- the machine's
`~/.agents/agents.md`, then `AGENTS.md` beside the session's directory -- and both reads
left with that template. The machine's own standing rules moved to a skill
(`~/.agents/skills/whiting-dev`), which is where this machine keeps every other
procedure: listed by `index_skill`, read when a task matches its description.

What came back on 2026-11-02 is narrower, and it comes back by a different road: the
session's own `AGENTS.md` is a *root* of the prompt index (`meta::RULES`), so it lands in
`instructions` behind the index rather than being concatenated onto the prose here. Three
things follow from that, and they are why this is not the deleted read returning:

- **One file, in the directory the shell stands in.** No machine-wide copy, no ancestor's.
- **It is compared, ordered and stored with the other roots.** The index plans the order
  (`prefix_cache::plan`), the row keeps the bytes, and a change appends a row instead of
  quietly altering what was sent.
- **A message that names it stays literal**, because it is also a spelling in the index --
  one file is never carried twice.

The older argument still holds where it applies: prose in every request whether or not
the turn needed it is what a skill exists to avoid. It is why the machine's rules stayed
a skill, and why the directory's rules -- which are about the work in that directory, and
which every turn there is done under -- are read from that directory and not from home.

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
