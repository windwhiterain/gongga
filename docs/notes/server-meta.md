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
(`docs/notes/server-pictures.md`), which is what a row can still show years later.
Before the request is written, `with_file_ids` replaces it with the File ID the
upload minted, which is what a provider can read.

## indexed on the first turn: the root set

The table above is what a reference becomes when it is expanded *in place*. It is not
what the first turn does. A session's first prompt row carries an index: the `AGENTS.md`
its directory keeps (`roots`'s `RULES`), every reference its messages name, and
everything those name in turn, as `spelling` and the text it stands for, appended to the
system instructions. Those mentions are then left literal in the message, because the
literal spelling is the key into the index.

The reason is the prefix cache. Whatever is in the system instructions is charged once
and then reused by every later request of that conversation; whatever is inline in a
message is re-sent as part of the transcript from that point on. A skill body is the
one thing whose size makes that difference matter: SPEC asks for the metadata to be
appended in place (`append path metadata to it`) and for the body to ride the index,
so `@skill:` expands to a pointer rather than to prose.

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

`literal` decides which mentions stay raw: the spellings the stored row's index
carries, or -- for a message delivered before that row exists -- the spellings the row
is about to carry, which is `roots` of the message. Everything outside that set is
resolved as it always was, and that is not a detail: `@image:` is never indexed (a
picture is not text), so it must still be uploaded and attached, or a first message
that pointed at a screenshot would lose it. An unknown skill is the same shape -- no
text to index, so it answers with its path instead of a hint about a file.

`spelling` is one function because it must be one spelling. The index carries it and
`expand` matches against it, so two spellings of one reference would leave the message
expanded *and* its text in the index. `@skill: heir` and `@skill:heir` are one
reference, and the canonical form is the one with no space.

## the queue, and why the index is not only the transcript

A message delivered while the session has no prompt row yet is left literal, on the
promise that the row about to be written will index it. That promise is only true if
the row reads the inbox too, so `agent::index_of` takes `store.user_texts` (the
transcript) *and* `store.queued_texts` (the messages still waiting there). Without the
second, a message queued during the first turn would have been stored with a literal
`@skill:` that no index entry ever backed -- the reader would see a name with nothing
behind it.

## the index's prose is the config's

The sentence introducing the entries is a `[prompts]` template, `reference_index`,
required like every other word the model reads. It was a `const` in this module for one
commit, which is exactly the thing `prompts::REQUIRED` exists to prevent -- and the
startup check caught it: a build whose pin did not yet carry the template refused to
start rather than sending a prompt with a hole in it.

## which message's references reach the index

The index is rebuilt on every request (`agent::index_of`, see `server-agent.md`), so
"when was this named" decides whether it is in there, and the answer is not the same
for the three kinds. Measured against a fake provider (`.dev/index-reach/probe.sh`):

| named in | `@file:` | `@skill:` |
| --- | --- | --- |
| the session's first user message | in the index, mention stays literal | in the index, mention stays literal |
| a later message | in the index from the next rebuild on (one new prompt row) | the same: the mention keeps its spelling, so the next rebuild reads the body in |
| the `Context notice`, before it is even delivered | -- the queue is read too, so its `@skill:` joins at the next rebuild | same |

One more row, because skills name each other: a root's own mentions are walked, so a
first message naming `@skill:chain` whose body says `@skill:leaf` carries both bodies.
That is what lets the shipped chapter `ship/skills/prefix-cache` be referenced from four
other places without being copied into them.

The two behave alike, and the reason is `expand`'s: it appends the metadata and leaves
the mention in the row, so the spelling that keys the index survives for a later
rebuild. Until 2026-11-03 it replaced the mention with prose instead, so a skill named
after the first message never reached the index and the row kept a description that
went stale the moment the skill changed.

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

## a reference that cannot be resolved stays text

An unreadable picture, a skill this machine does not have, a picture named for a
model whose roster entry says no images: each one leaves the reference exactly as
written. A message is never refused because one of its references was stale, and
the model reads "there was supposed to be a picture here" instead of a hole in
the sentence.

An upload is not part of that list. It is the one step whose failure loses nothing:
the copy is already in this home, the row keeps it, and `image::content_part` sends
those bytes when there is no file id. A File API that is down costs a bigger request.

## the upload, and what happens without it

The File API call happens at delivery, so the request carries the id rather than
the bytes. It is an optimisation with a fallback, not a requirement: an upload
that fails leaves the row's picture without a file id, and `image::content_part`
inlines the copy when it writes the request. An unreachable File API costs a
bigger request and never the message.

Delivering costs one provider round trip per picture. That is the price of "in
advance" in the SPEC: it buys a transcript that replays without touching the
file again.
