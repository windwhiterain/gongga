# server-migrate

A home carries a version, and a build carries the version it expects. The gap is
what this module closes, before the database is opened.

## the_version_file_is_the_state

`<home>/version` holds the version a home was last migrated to, and a missing or
unreadable file is version 0: the shape the home was created by. The file is
written *after* a migrator exits zero and never before, so a start that dies
mid-migration runs the same step again. Every migrator has to be safe to run
twice for that reason, and the shipped ones are.

## a_migrator_is_fetched_by_path

The migrators are the one thing the install pass does not place, because a home
two versions behind asks for the step it is missing, not for whatever the current
pin happens to ship. `migrate/<from>-to-<to>.py` is looked for in the pinned
commit first, then in the newest commit of the cache that carries the path, then
in a local checkout the pin names. It is written into `<home>/migrate/` so the
next start runs the same bytes, and executed with `--home` and `--db`.

## an_interpreter_is_asked_not_looked_for

`python3` on a Windows PATH can be an execution alias that exits 9009 because the
Store app behind it was never installed. A name that resolves is therefore not a
Python: each candidate is run with `-V` and the first that answers is the one the
migrator gets. The failure this prevents was a migration that never ran and a
server that would not start, with only an exit code to say so.
