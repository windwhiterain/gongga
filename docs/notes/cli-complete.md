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
