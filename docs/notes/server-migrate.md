# server-migrate

A home carries a version, and a build carries the version it expects. The gap is
what this module closes, before the database is opened -- and before the shipped
content is installed, which is the order that keeps a home's data from depending
on a network: see `docs/notes/server-lib.md`.

## nothing_is_copied_first

There is no backup. A migrator rewrites in place, and a step that succeeds while
doing the wrong thing leaves a home at the version it claims with no second copy
of its data -- that is the cost of not carrying a copy nothing verifies and
nothing cleans up. What stands in its place is the requirement that a step is
safe to run twice and is tested against the shapes it will meet, and a script
that touches something it cannot redo is free to copy that thing itself: it
receives `--home`, and it knows what it is about to change.

## a_home_with_no_database_is_stamped

With no database there is nothing to bring forward: the home is being started for
the first time and the store creates the schema. Asking for a migrator there
would fail on every new home, so the version is written and the chain is skipped.

## a_migrator_is_fetched_by_path

The migrators are the one thing the install pass does not place, because a home
two versions behind asks for the step it is missing, not for whatever the current
content happens to ship. `migrate/<from>-to-<to>.py` is looked for in the resolved
commit first, then in the newest commit of the cache that carries the path, then
in that checkout's history. It is written into `<home>/migrate/` so the
next start runs the same bytes, and executed with `--home` and `--db`.

## a_column_with_no_reader_is_dropped_where_the_schema_heals

`sessions.monitor_attempts` went when the model search it counted went, and it is
not a versioned step. A version bump would make every home's *start* depend on a
migrator being fetchable, and the start resolves content without fetching (`ship::resolve`)
-- a home that had not yet seen the new content would refuse to start over a column
nothing reads. Dropping it is order-free and idempotent, so it belongs with the
shape healing the store already does at every open (`store::migrate`, which also
renames the columns two builds disagreed about), and a build that reads the column
still adds it back on its own open.

## an_ordered_rewrite_is_a_versioned_step

The 5-to-6 step moves each session's head into reserved slots: the prompt to
seq 1, the tool schema to seq 2, the rows they displaced following in order.
That is a rewrite of rows already sent, not a column heal -- it is order-dependent,
so it cannot live in `store::migrate`, which runs on every open and must be
order-free. It drops `context_no_update` for the duration and leaves the server
to recreate it with `CREATE TRIGGER IF NOT EXISTS` when it opens; a migrator runs
before that open. Only the head region moves: monitor marks and compact
checkpoints name seqs far past it, so they keep pointing at the same rows.

## an_interpreter_is_asked_not_looked_for

`python3` on a Windows PATH can be an execution alias that exits 9009 because the
Store app behind it was never installed. A name that resolves is therefore not a
Python: each candidate is run with `-V` and the first that answers is the one the
migrator gets. The failure this prevents was a migration that never ran and a
server that would not start, with only an exit code to say so.
