# `crates/server/src/meta.rs`

A message may name three things with an `@` reference, and each one means
something different:

| written | becomes |
| --- | --- |
| `@image: <path>` | the mention plus the id of its copy: `@image: <path> (id)`, with the picture on the row |
| `@skill: <name>` | its spelling kept, with the `SKILL.md` it names: `@skill: <name> (<path>)` |
| `@file: <path>` | nothing: a mention, left exactly as written |

Keeping the spelling is what makes the index work, and it is why `@file:` is left
alone entirely: without a spelling to match on, `@file: x` would have to be guessed
at, and a message about a picture would attach a picture the user only meant to talk
about. The prefix decides, not the file's extension.

The `id` a picture carries is not the provider's: it is the copy this home keeps
(`docs/notes/server-pictures.md`), which is what a row can still show years later. Nothing
replaces it in the text, because a provider's file is not something a row may record: it
expires. A request resolves the copy to whatever file the provider holds *now*, and that
is the only value in the body that is looked up rather than copied.

## indexed on the first turn: the root set

The table above is what a reference becomes when it is expanded *in place*. The first
turn in addition builds an index: a session's first prompt row carries the `AGENTS.md`
its directory keeps (`roots`'s `RULES`), every reference its messages name, and
everything those name in turn, as `spelling` and the text it stands for, appended to the
system instructions.

The reason is the prefix cache. Whatever is in the system instructions is charged once
and then reused by every later request of that conversation; whatever is inline in a
message is re-sent as part of the transcript from that point on. A skill body is the
one thing whose size makes that difference matter: SPEC asks for the metadata to be
appended in place (`append path metadata to it`) and for the body to ride the index,
so `@skill:` expands to a pointer rather than to prose, on the first turn too.

That pointer is not a courtesy. The index carries a skill's body and nothing about
where the file is, so a first message that kept only `@skill: <name>` sent the model
hunting the three roots for the directory; measured on 2026-11-04 in one session, four
`ls`/`skill` calls before it found `SKILL.md`. `@file:` is the opposite case: its
spelling *is* its path, so it is left exactly as written and still keys the index.

## the rules file is a root, not a message

`AGENTS.md` in the session's own directory is the walk's first spelling, named by
`roots` rather than by a message: what a directory keeps as its rules is standing
instruction for the work done in it, and a session that has to be told to read it has
already spent the turn it was meant to save. It is one file, and no ancestor's copy:
`cwd/AGENTS.md` and nothing above it, so a subdirectory with none of its own inherits
nothing.

Where it lands is the whole point. The index sits in `instructions`, in front of the
transcript, so the rules are charged once and re-used by every later request of the
conversation; a rules file expanded into a message would ride the transcript instead and
be re-sent for the life of the session. Being a spelling also means a message that writes
`@file: AGENTS.md` stays literal and keys the same entry, so one file is never carried
twice.

It is read on every request, like every other root, so an edit reaches the next turn --
at the price the index always charges for an edit: a new prompt row, and the prefix behind
it re-billed.

Where it is read changed on 2026-11-02, and the reason is the SPEC invariant it broke. It
used to be read from whatever directory the shell reported for that turn, and that is not
the session's directory: `SessionHandle::cwd()` is the shell's position, refreshed with a
`pwd` after every command. A `cd` inside a tool call therefore read another directory --
or none -- and appended a new system row, so the map disappeared exactly when a session
began to explore. Measured: 21,400 → 12,017 → 21,400 characters across three rows of one
turn, all caused by `cd`.

So `roots` now reads everything in one directory: the session's, the one the prompt row
records. Not a hybrid -- the first attempt kept the shell's directory for references a
*message* names, and a message naming `@file: docs/notes/tui-main.md` lost that root the
moment the shell `cd`d away. A relative path in a sent message means one file for the
life of the session; that is what "already sent" has to mean for a map carried in the
prompt. `scripts/prompt_selftest.sh` holds the line.

Two consequences, both accepted:

- a session whose directory has no `AGENTS.md` reads none, even if its shell later `cd`s
  somewhere that has one. To reach another directory's rules, name them:
  `@file: /that/dir/AGENTS.md`.
- a reference in a message is relative to the session's directory, not to wherever the
  shell stood when the message was typed. Absolute paths and references written before a
  `cd` are unaffected; a model that `cd`s and then names `src/x.rs` means `home/src/x.rs`,
  so it should write the full path.

## `roots` is the walk

`roots` is the walk: breadth first, first seen, once each. A file that names itself, or
two files that name each other, is two entries rather than a loop, because `seen` is
consulted before every enqueue. A reference the walk cannot read -- a picture, which is
not text, or a skill this machine does not have -- is left out entirely rather than
invented: its mention stays literal in the message, so the reader sees a name with
nothing behind it, which is the truth.

`@image:` is never indexed (a picture is not text), so it is copied and attached
wherever it is named, or a first message that pointed at a screenshot would lose it.
An unknown skill is the same shape -- no text to index, so it answers with its path
instead of a hint about a file. `@skill:` is neither: what the index holds is its
body, and a body does not say where the file is.

`spelling` is one function because it must be one spelling. The index carries it and
`expand` writes it back into every `@skill:` mention, so a message naming a skill a
second time keys the entry already there. `@skill: heir` and `@skill:heir` are one
reference, and the canonical form is the one with no space.

## the queue, and why the index is not only the transcript

A message delivered while the session has no prompt row yet keeps its `@file:` mentions
as written, on the promise that the row about to be written will index them. That
promise is only true if the row reads the inbox too, so `agent::index_of` takes
`store.user_texts` (the transcript) *and* `store.queued_texts` (the messages still
waiting there). Without the second, a file queued during the first turn would be named
by a message and carried by no entry.

## the index's prose is the config's

The sentence introducing the entries is a `[prompts]` template, `reference_index`,
required like every other word the model reads. It was a `const` in this module for one
commit, which is exactly the thing `prompts::REQUIRED` exists to prevent -- and the
startup check caught it: a build whose pin did not yet carry the template refused to
start rather than sending a prompt with a hole in it.

## which message's references reach the index

`agent::index_of` runs on the request that writes the session's prompt row, and
`prompt_in_force` returns that row from then on (see `server-agent.md`), so "when was
this named" decides whether it is in there at all:

| named in | `@file:` | `@skill:` |
| --- | --- | --- |
| the session's first user message | in the index; the mention stays as written | in the index; the mention becomes `@skill: <name> (<path>)` |
| a message already queued when that row was written | the queue is read too, so it is in the index as well | same |
| any later message | nothing: the mention stays a literal path with no entry behind it | expanded to `@skill: <name> (<its SKILL.md>)`; the body is not in the index, so the reader opens the file itself |

One more row, because skills name each other: a root's own mentions are walked, so a
first message naming `@skill:chain` whose body says `@skill:leaf` carries both bodies.
That is what lets the shipped chapter `ship/skills/prefix-cache` be referenced from four
other places without being copied into them.

The first two rows are why the writing rule is a rule and not a preference. Measured at
`52c180a` with a fake provider (`scripts/subagent_selftest.sh`, scenarios 0 and H): a
session whose first message named `@file: early.md` kept
`rooted = ['@file:early.md']` through three turns, its later message's `@skill: late`
was stored as `@skill: late (<path>)`, and the request behind that turn carried `EARLY`
and no `LATE`. One `system` row, not one per request.

Until 2026-11-03 this section said the index was rebuilt on every request, and that a
later message joined it on the next rebuild. That was `index_of`'s rule before the head
was stamped, and the stamped head is the answer to a measured bill: a 24k-character
index edited by one line cost one session 292,992 cached tokens (`server-agent.md`).

What a reader should take from this is a writing rule, not a limitation: name what the
work needs in the first message. `ship/skills/prefix-cache` holds that rule, the
spelling table, and the examples -- for a reader who is about to write a brief or a
lesson.

## why the value is one word

`@image: shot.png` is a prefix and the word after it, or `@image:shot.png`
written together. That is exactly what the completion popup inserts, so what a
reader completes is what the parser reads. A path with a space cannot be named;
`@file:` is the way to write about one.

Trailing punctuation belongs to the sentence: `(@image: shot.png).` names
`shot.png`. A prefix with nothing after it is prose, and stays prose.

## which machine a reference is read on

One session may have shells on two machines, and a reference is read on the machine
that produced the text it is written in: a tool's output on the machine that ran the
tool (`Tool::machine`), a user's message on the machine the session stands on. Never
on whichever machine happens to have a file with a similar name -- that is how a
Windows spelling of a peer's path once made this server read a *different* checkout's
`AGENTS.md` into the prompt.

The session's directory belongs to one machine, and it is the *placement* -- what
`set_session --cwd` last said -- not where its shell happens to stand: a `cd` moves a
shell and not a session, so it is not a rewrite of what was already sent, while a move
is, and a move therefore writes a new prompt row. The old row keeps the old directory,
which is what "already sent" means.

A path is joined the way the reading machine joins paths, so a peer's `C:\Users\...`
and a Linux `/home/...` each stay what they are. A reference that cannot be read is
read on no other machine: it stays literal.

See `.agents/notes/2026-11-02-a-session-has-two-machines.md`.

## expanded once, at delivery

`expand` runs in `commands::deliver`, before the row is written. The stored row
therefore never carries a reference a later request would resolve differently: a
skill keeps the `SKILL.md` path the name resolved to (what that file says may change
afterwards, exactly as it may for `@file:`), and an image keeps the file id it was
uploaded as.

That is also what makes the pending-inject path carry the same thing: the payload
holds the expanded text and the pictures (`Expanded::to_json`), so a message
queued behind a running turn arrives as it was sent, not as the machine looks
when the turn gets round to it.

Every replacement keeps text now -- a skill's pointer, a picture's mention and id --
so none of them touches the space in front of it. It used to: a picture was replaced
by nothing, and the rule that closed the sentence up around that hole was applied to
every mention, which glued `and @skill: late` into `andskill` until 2026-11-03.

Expansion is idempotent, by the suffix each kind appends: a `@skill:` mention
already followed by ` (its SKILL.md's path)`, or an `@image:` mention by
` (the stored copy's id)`, is left exactly as it stands. Stored rows are
re-sent verbatim -- the monitor's retry carries the report it was given,
unchanged -- and without the guard every such pass appended the pointer a
second time, so the row the reader saw was `@skill: monitor (path) (path)`.

## a reference that cannot be resolved stays text

An unreadable picture, a skill this machine does not have, a picture named for a
model whose roster entry says no images: each one leaves the reference exactly as
written. A message is never refused because one of its references was stale, and
the model reads "there was supposed to be a picture here" instead of a hole in
the sentence.

An upload is not part of that list. It is the one step whose failure loses nothing:
the copy is already in this home, the row keeps it, and `image::files` sends those bytes
when there is no file id. A File API that is down costs a bigger request.

## the upload, and what happens without it

The File API call happens when a request is written, not when a message arrives, and
that is the difference between a transcript and a log of one: a file the provider minted
is deleted by the provider, so the id is looked up at the moment it is needed and a
stale one is replaced. Delivering no longer costs a round trip; the first request that
needs a picture does, and every later one reuses the file while it has life left.

It is an optimisation with a fallback, not a requirement: an upload that fails makes
`image::files` inline the copy instead. An unreachable Files API costs a bigger request
and never the message.
