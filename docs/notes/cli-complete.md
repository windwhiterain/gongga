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
right one. The source therefore follows the line: a value after `--target` that
the config holds sends it to the target, then the machine the line's session runs
on decides, and only a line with neither reads here.

The split that makes this possible is the one completion already lives by. The
server publishes the names a config holds as the target argument's `Static`
list; the client reads that list off the catalog instead of the config file, so
the names are never in two places. It also means "which machine?" is answered by
the same data the popup offers.

A line names its machine in two ways, in this order: `--target`, and then the
target of the `--session` it names -- or, when it names none, the target of the
session the sticky arguments send its message to. The directory travels with the
machine: `--cwd sub` on a session standing in `C:/work` is completed under
`C:/work`, which is also where the server reads it
(`session::stated_directory`). The second exists because of a measured
failure: completing `--cwd` on a session already running on a peer, with no
`--target` on the line, offered **this** machine's directories -- and typing an
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
