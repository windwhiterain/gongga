# `crates/server/src/meta.rs`

A message may name three things with an `@` reference, and each one means
something different:

| written | becomes |
| --- | --- |
| `@image: <path>` | an attachment: the reference leaves the text, the picture rides the row |
| `@skill: <name>` | the metadata of that skill: its description and where its `SKILL.md` is |
| `@file: <path>` | nothing: a mention, left exactly as written |

The third is the point of having a spelling at all. Without it, `@file: x` would
have to be guessed at, and a message about a picture would attach a picture the
user only meant to talk about. The prefix decides, not the file's extension.

## indexed on the first turn: the root set

The table above is what a reference becomes when it is expanded *in place*. It is not
what the first turn does. A session's first prompt row carries an index: every
reference its messages name, and everything those name in turn, as `spelling` and the
text it stands for, appended to the system instructions. Those mentions are then left
literal in the message, because the literal spelling is the key into the index.

The reason is the prefix cache. Whatever is in the system instructions is charged once
and then reused by every later request of that conversation; whatever is inline in a
message is re-sent as part of the transcript from that point on. A skill body is the
one thing whose size makes that difference matter, and SPEC already asks for the body
(`replaced as skill content with wrapper`) rather than a line of metadata.

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

## why the index is built once

`agent::index_of` runs only when there is no prompt row yet, and every later row copies
what the first one decided. Growing the index later would be correct and expensive: it
sits in front of the whole transcript, so adding a section re-bills everything behind
it. A reference named in a later message is expanded inline as before -- the index is
the first turn's, the row that follows is history.

## why the value is one word

`@image: shot.png` is a prefix and the word after it, or `@image:shot.png`
written together. That is exactly what the completion popup inserts, so what a
reader completes is what the parser reads. A path with a space cannot be named;
`@file:` is the way to write about one.

Trailing punctuation belongs to the sentence: `(@image: shot.png).` names
`shot.png`. A prefix with nothing after it is prose, and stays prose.

## expanded once, at delivery

`expand` runs in `commands::deliver`, before the row is written. The stored row
therefore never carries a reference a later request would resolve differently: a
skill that was installed when the message was sent keeps the description it had,
and an image keeps the file id it was uploaded as.

That is also what makes the pending-inject path carry the same thing: the payload
holds the expanded text and the pictures (`Expanded::to_json`), so a message
queued behind a running turn arrives as it was sent, not as the machine looks
when the turn gets round to it.

## a reference that cannot be resolved stays text

An unreadable picture, a skill this machine does not have, a picture named for a
model whose roster entry says no images: each one leaves the reference exactly as
written. A message is never refused because one of its references was stale, and
the model reads "there was supposed to be a picture here" instead of a hole in
the sentence.

## the upload, and what happens without it

The File API call happens at delivery, so the request carries the id rather than
the bytes. It is an optimisation with a fallback, not a requirement: an upload
that fails leaves the row's picture without a file id, and `image::content_part`
inlines the bytes when it writes the request. An unreachable File API costs a
bigger request and never the message.

Delivering costs one provider round trip per picture. That is the price of "in
advance" in the SPEC: it buys a transcript that replays without touching the
file again.
