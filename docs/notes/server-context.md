# What a session sends is a stored sequence

A request's context is a list of rows read from the database in order: no filter,
no substitution, no decision. The decision was made when the rows were written,
which is the only place a rule can be checked and the only place it can be paid for
once.

## The rows

`context_rows`, keyed by `(session_id, generation, position)`. A row carries the
item exactly as it goes on the wire, the message it came from, and the pictures it
named. Appending a message appends one row to the generation now in force.

A row also says where its item goes: `slot = 'input'` is an element of the request's
message array, and `slot = 'field'` is a provider field named in `field` -- the
instructions, the tool set. Both are items of the one sequence, in the order they
were written; a request neither places one nor spells one. A field is read by its
last write in the generation in force, so a later write is the one that stands.

The head of the first request is written where it is decided: the instructions as a
field item, then the opening, the separator, the index and the card as input items.
Nothing renders them again, which is why every request of a lineage has the same
head -- the same bytes, and the provider's cache with them.

## A move

A session that moves to a peer offers tools the machine it left does not:
`remote_bash` replaces `bash`. That is not a new lineage and not a refusal -- the
move appends one tool-set field item, which takes effect by being later, and one
message saying where the session now stands. Every byte written before those two
rows is still sent, unchanged, and the request after the move is the sequence with
two more items in it.

## The generation

A compaction commit writes the next generation inside its own transaction: the rows
up to the checkpoint boundary are copied byte for byte, the summary takes the place
of the segment it replaces, the retained round follows, and the generation before
it is left exactly as it was. A row keeps its slot in the copy, so a field stays a
field. A request then reads whichever generation is the latest -- so a commit
changes what the *next* request sends and never what an earlier one sent, which is
also why the prefix's bytes survive and the provider's cache with them.

## Homes written before this

`backfill_context_rows` runs at startup for any session that has messages but no
rows, and applies the rule that home was read under -- columns up to its stored
prefix end, its checkpoints' artifacts, everything from the latest round on -- once,
writing generation 0. A prompt and a tool set are payloads, so the head's items are
written from them here too; the messages' items are the ones that were stored. Its
next request therefore sends what its last one did. A session that already has rows
is left alone, which is what makes it safe on every open.
