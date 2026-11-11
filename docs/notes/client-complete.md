# cli/complete.rs

## complete.rs

These types live here, in the command layer, because completion is about
commands: the catalog decides what a token could become. The front-end renders
the popup and inserts the chosen text, but it supplies no knowledge of its own
-- which is what lets `tui` remain ignorant of commands while still offering Tab
completion.

## Candidate::space

False for a directory, so that Tab again descends into it instead of starting a
new word: the insertion does not end the token, it stays inside it.

## Completion

The `selected` index lives here rather than in the front-end because it is part
of the same decision: a popup whose contents and whose current choice were
tracked separately could disagree, and the disagreement would show up as the
wrong text being inserted. `current` returns `None` rather than panicking when
that index no longer addresses a candidate, so the two can be replaced
independently without the popup needing a repair step.

## Completer::complete

An empty vector means "nothing to offer", which is how the popup stays closed
for free-form values without the front-end having to know which values are
free-form.

## SharedCompleter

The front-end holds it, the command layer swaps the catalog inside it whenever
the server publishes a new one -- which is why the handle is shared rather than
copied.

## token_bounds

Completion is decided by *which token* is under the cursor, and the candidate
source (the catalog) is here, so the split lives here too. The front-end uses
the same split when it applies a candidate, which is why this is shared rather
than duplicated: two answers to "where does this token start" would drift, and
the drift would show up as text inserted in the wrong place.

A cursor past the end is clamped rather than rejected, so a stale cursor coming
from a front-end that redrew in the same tick yields the last token instead of a
panic.

## A path on the machine the line names

`--cwd` on a remote session is a path on that machine, so offering this one's
filesystem is not a hint — it is a wrong answer the operator cannot tell from a
right one. The source therefore follows the line: a value after `--remote` that
the config holds sends it to the target, then the machine the line's session runs
on decides, and only a line with neither reads here.

The split that makes this possible is the one completion already lives by. The
server publishes the names a config holds as the target argument's `Static`
list; the client reads that list off the catalog instead of the config file, so
the names are never in two places. It also means "which machine?" is answered by
the same data the popup offers.

A line names its machine in two ways, in this order: `--remote`, and then the
target of the `--session` it names -- or, when it names none, the target of the
session the sticky arguments send its message to. The directory travels with the
machine: `--cwd sub` on a session standing in `C:/work` is completed under
`C:/work`, which is also where the server reads it
(`session::stated_directory`). The second exists because of a measured
failure: completing `--cwd` on a session already running on a peer, with no
`--remote` on the line, offered **this** machine's directories -- and typing an
absolute peer path then looked like "absolute paths are not supported", since
`C:/` is absolute on both machines and the entries were merely the wrong ones.
`SessionInfo` now carries `target`, so the client knows where a session is
without being told twice.

The ask cannot happen inside the completer: `Completer::complete` is synchronous
and runs in a key handler, while a peer's directory is a network away. So the
completer answers from what has arrived, records what it wanted, and the front-end
sends that after the step — `answer_completion` in `tui/run.rs`. An answer bumps a
version the window compares; that is the whole repaint signal, so the contract
between the two stays one integer.

Two consequences worth naming. First, the first Tab on a directory shows nothing
and the popup fills when the answer lands: a listing is never a reason to block a
keystroke. Second, an answer stands for `REVALIDATE` before it is asked for again,
because the repaint re-asks — an answer that were always stale would turn every
repaint into another round-trip, and the refresh into a loop.

## Offered — the candidates and what they are for

A completer answers with both halves at once, because the second is only knowable
where the first is decided: `subject` names the argument whose value the candidates
complete (`--cache_tier`), and is `None` when the answer is a command name, a flag
name or an `@` reference. The popup draws it as its title, so a list of values is
never read as some other list -- `5m` under `--cache_tier` is a tier, and the same
word under `--ctx_limit` would be a percentage.

It travels inside the answer rather than as a second question, so a front-end asks
the catalog once per keystroke: a separate `subject(line)` call would parse the line
twice and could disagree with the list it labels.

## Completing inside a structured value

An argument may publish a shape (`ArgSpec::structure`, SPEC's `SessionConfig`):
`--config` is one JSON object, and the whole of it is a single shell word, so the
token rules above see one unreadable word and offer nothing. What the operator
needs there is the names at the depth the cursor stands in, and the values of the
field being filled.

**Where the value's text is.** From just past the quote that makes it one word to
the closing one. Only a quoted value is read: an unquoted object reaches the
server as prose, because the shell split strips its quotes, so completing inside
one would help write a line that cannot parse. A `--` that names no argument of
the command is not a flag, which is how a quoted value may spell one.

**A place, not a parse.** `client/src/json_value.rs` walks the text once and
answers where the cursor stands: the path of field names from the root, whether
the cursor names a field or fills one, what has been typed of that token, and the
fields of the enclosing object that are already written. The value is half typed
by definition, so nothing there is an error: text it cannot place -- an array, a
closed object, a value that never opened -- is `None`, which reads as "offer
nothing". A cursor past the end is clamped, never refused.

**What a candidate replaces.** The whole JSON token: from its opening quote to
its closing one. A fragment left behind would be the head of the value; a quote
left behind would close the insertion in the middle. So `Offered` carries an `end`
as well as a `start` (`token_bounds` cannot answer it: the token ends where the
JSON says, not at the next space), and the front-end takes both. Accepting at
`{"model":"deepseek-fl|"}}' --model x` replaces exactly the quoted fragment and
leaves the rest of the line as it was.

**What it inserts.** A whole token, so that what the popup leaves behind is JSON
the server could read if the object were closed:

- a field name is `"name":`, plus the `{` that opens its place when the field
  holds an object -- what follows a name is not in doubt, and a name alone is not
  JSON. A field the object already states is not offered again.
- a leaf value is JSON-quoted when it is not a literal, decided from the text the
  candidate carries: `keep_alive` takes `true` and `ctx_limit` takes `"70%"` from
  one list of texts, because quoting is this layer's decision and not the
  publisher's.
- a `model_ref` leaf is the object SPEC spells `Model := {provider, model}`,
  split out of the roster's `<provider>/<model>`.
- an empty value gets the brace that starts the object, because a name outside an
  object is not JSON.

The closing braces are the operator's: a candidate ends where its own token ends.
Nothing is offered where an object field's value goes (its name wrote the brace),
in a list, or after the object has closed.

**Which model a value belongs to.** `accepts` is keyed by model, and a config
states its own: a value inside `model_config` reads the `model` its enclosing
object already carries -- in either spelling, `"deepseek-flash"` or
`{provider, model}` -- before the line's `--model` or the server's default is
asked. Offering another model's levels would be a list the server refuses.

**The words of a line that cannot be split.** A line whose value is still open is
not a shell line, so `words_of` closes that one quote and splits again: what the
rest of the completion needs from the words is which command, session and machine
the line names, and those are behind the quote, not inside it. Without that, the
whole popup of a line being typed mid-quote would be empty.
