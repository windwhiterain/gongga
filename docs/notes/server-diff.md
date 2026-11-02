# `crates/server/src/diff.rs`

## module doc

One question: how much did a text change, and can the change be said in fewer lines than
the text? The answer is a unified patch -- the three-line-context, `@@`-headed shape a
reader already knows from `git diff` -- or nothing at all, because a change larger than
the caller's tolerance is not worth saying that way.

It exists for one caller: an entry of the prompt index whose file was edited. Re-sending
the index re-bills everything behind the entry, and the provider holds that prefix only
in the old shape; keeping the entry as it was sent and appending the change is the only
version that keeps the whole prefix warm. See `docs/notes/server-prefix-cache.md`.

## the measure, and what it is compared with

`removed.max(added)`, against the old text's line count. The larger of the two, not the
sum: one edited line is one line, and counting it twice would silently halve every
tolerance a caller sets. The denominator is the text that was sent, because that is the
text the provider holds -- a change is large or small relative to what has to be
corrected.

Over tolerance is `>=`, not `>`: at a tolerance of `1.0` a text rewritten end to end is
exactly as large as itself, and it must still be re-sent. The comparison is
`changed / old` in other words, so the boundary has to fall on the side of re-sending.

An empty old text against a non-empty new one is over tolerance whatever it costs: there
is no prefix to keep warm, and nothing to diff against.

## the script: head, tail, and an anchor

Trimming what both sides still share at the head and the tail is free and covers the
common edit. What is left between them is one region, and the question is whether it can
be split: a line that occurs *once* in each side's region can only pair with itself, so
it is a safe place to cut. The candidate nearest the middle of the region wins, and the
two halves are compared the same way.

Ambiguous lines -- blank ones, `---`, a closing brace -- never anchor, which is the whole
reason for the uniqueness test: they would pair with the wrong occurrence and claim a
share the two texts do not have.

Recursion would be the natural shape and is not used: the pending regions live in a
stack, so a file with one changed line per hundred costs a stack entry per change rather
than a stack frame per region. It is also why output order is built by pushing the
right-hand region before the left one.

## reproduces: a patch is checked before it is used

The script is applied back to the old text and must reproduce the new one exactly;
otherwise the patch is dropped and the caller re-sends the whole entry. This is not
guarding against a hypothetical: the reader of a patch is a model, and a patch that does
not apply is not an error message there but a false statement about what the file says.
The check is one pass over the lines, and it is what lets the algorithm above stay as
simple as it is.

## CONTEXT

Three lines, which is what `diff -u` defaults to and what a reader expects. A constant
rather than a knob: it decides how much a hunk repeats, while the caller's tolerance
already decides how large a change may be before the patch is abandoned.

## what it is not

- Not a `git apply`-compatible patch: no `---`/`+++` header, no index line. Its reader is
  a person, and the entry's spelling stands where the file name would.
- Not word-level. A line is the unit, because a line is what an editor and a reader both
  see, and because the measure is compared against a line count.
- Not a similarity score. Nothing is "70% similar": there is a patch, or there is not.
- Line endings are not normalised. A file whose every line gained a `\r` is a file whose
  every line changed -- the honest reading, and one that lands on the re-send side.
