# The rows a script found are gone after a reread

`opencode-go` disappeared from the model picker, and it stayed gone. Writing the
key into `~/.agents/keys.toml` did not bring it back -- it made it worse: that
write is a roster edit, and every roster edit emptied the provider.

## What the roster is

`providers.toml` states the static `[[provider.model]]` rows. A relay whose
catalogue moves with the subscription names a `models_script` instead, and its
rows arrive later: `spawn_scripts` runs the script on a thread, `merge_scripted`
joins what it found to the catalog in force, bumps the generation, and the
republisher sends the new table to every client. One roster, learned in two
steps -- that part is deliberate and documented.

## What breaks it

`roster()` re-reads when the roster or `keys.toml` changes, and `read_fresh`
rebuilds from the file alone. For a scripted provider the file has no rows at
all, so the reread produces a provider with zero models.

The intent was that the script would put them back, but `spawn_scripts` decided
"already ran" by `slot.is_some()`, and a `JoinHandle` stays in the slot after the
thread ends. So the first reread after startup found the handle still there,
returned, and every later reread did the same. `opencode-go` stayed empty until a
restart.

Two paths make it look intermittent. A reread that lands *while* a script is
still running gets its rows anyway, because `merge_scripted` joins to whatever
catalog is in force when it ends. And `settle_scripts` takes the handle, which
also resets the slot. So the bug shows up exactly when the script finished before
the edit -- normal startup, not the test.

## The fix

`SCRIPTED` is the generation whose rows are already in hand, set in
`merge_scripted` to the catalog's generation after the join. `spawn_scripts` runs
when `SCRIPTED < catalog.generation()` and returns otherwise. A reread therefore
starts a new run; a thread still running from an older roster is left to finish,
because its rows land on the current catalog anyway.

## How it was checked

An isolated home (config copied, `ship` cloned from the repository's own
checkout) and an isolated `GONGGA_AGENTS_HOME` with a fake relay whose script
prints one model. Start, wait for the first merge, `touch keys.toml`, count
merges. Fixed build: 2 every time, five runs. The shipped build: 1 -- sometimes 2
when the touch lands early, which is the race above.

The first runs of that harness reported failures that were not failures:
`127.0.0.1:9741 is already in use` from the previous run, and the test read the
new process's early log as "the reread never ran the script". The harness now
refuses a busy port and each run takes its own.
