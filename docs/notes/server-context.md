# What a session sends is a stored sequence

A request's context is a list of rows read from the database in order: no filter,
no substitution, no decision. The decision was made when the rows were written,
which is the only place a rule can be checked and the only place it can be paid for
once.

## The rows

`context_rows`, keyed by `(session_id, generation, position)`. A row carries the
item exactly as it goes on the wire, the message it came from, and the pictures it
named. Appending a message appends one row to the generation now in force.

## The generation

A compaction commit writes the next generation inside its own transaction: the rows
up to the checkpoint boundary are copied byte for byte, the summary takes the place
of the segment it replaces, the retained round follows, and the generation before
it is left exactly as it was. A request then reads whichever generation is the
latest -- so a commit changes what the *next* request sends and never what an
earlier one sent, which is also why the prefix's bytes survive and the provider's
cache with them.

## Homes written before this

`backfill_context_rows` runs at startup for any session that has messages but no
rows, and applies the rule that home was read under -- columns up to its stored
prefix end, its checkpoints' artifacts, everything from the latest round on -- once,
writing generation 0. Its next request therefore sends what its last one did. A
session that already has rows is left alone, which is what makes it safe on every
open.
