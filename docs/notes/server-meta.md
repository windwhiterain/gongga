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

## tests::a_reference_is_its_prefix_and_one_word

Both spellings resolve to the same value, and the three prefixes do not shadow
each other.

## tests::a_prefix_matches_anywhere_and_punctuation_is_the_sentence

The bracket and the full stop around a reference belong to the sentence. An
email address is not a reference, and neither is a prefix with nothing after it.

## tests::a_file_reference_is_left_exactly_as_written

`@file:` is the control case: expansion leaves it alone, which is what proves the
other two are decisions rather than a blanket rewrite.
