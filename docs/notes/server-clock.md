# `crates/server/src/clock.rs`

## module doc

One thing the prompt says is what time it is. That is a claim the harness can only
make from its own clock, and the only interesting property is how coarsely: the text
lands in the last line of the system prompt, in front of the whole transcript.

## now, at

Minute precision, deliberately. A prompt that changed every second would re-bill the
transcript behind it on every step, which is the exact cost this repository spends
most of its caching work avoiding; a prompt that changed every day would answer "when
did this happen" with the wrong day. A minute is the unit a person reads a time in and
the unit a turn is worth paying for. A session that needs the second reads it from a
shell -- `date` is a tool call, not a prompt line.

`at` takes seconds since the epoch so the test can name instants; `now` is the same
function with the clock read once.

## civil

`civil_from_days`, Howard Hinnant's algorithm, transcribed rather than imported: the
alternative is a date crate for one line of output, and this repository already keeps
its own base64 for the same reason. The arithmetic is the whole implementation, so the
test names instants instead of describing the algorithm -- the epoch, a leap day, and a
year boundary.

## what is *not* here

No time zone. The tail says `UTC` and means it: a server that guessed at a zone would
be wrong for exactly the sessions that moved, and the reader can convert. No
localisation either -- one spelling, the one every other line of this server uses.
