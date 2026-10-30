# server/agents.rs

## agents

The machine's shared content, kept in a clone under `~/.agents`. Today that is
the roster, the quota scripts beside it, and the general bucket of skills under
`skills/<name>`; the module is named for the directory rather than for the files
because the next thing to live there will not be a roster, and a module renamed
every time is a module nobody cites.

## Roots

Two directories, and the shape inside them is the same, so a relative path means
the same thing in both:

| root | who writes it | what it is |
|---|---|---|
| `~/.agents/gongga-agents` | gongga, by cloning | the community repository: roster, quota scripts, general skills |
| `~/.agents` | the user | this machine's own files, when a field or a skill needs overriding |

The sameness is deliberate. `quota_query.script` in the roster is a path
relative to the file that names it (`providers/deepseek.py`), so a community entry
resolves into the clone and a private entry into `~/.agents/providers/`, with no table
of "where scripts live" to keep in sync with two layers that are allowed to
differ. A layout that buried the scripts one level deeper would have needed
that table for exactly one level of nesting and bought nothing.

## sync

Clone when absent, `git pull --ff-only` when present. Not `reset --hard` and not
a mirror: the clone is gongga's to *maintain*, not to own, and a reader who
edited a provider there to open a pull request would lose the edit at the next
hourly tick. A pull that cannot fast-forward is therefore one warning a reader
can act on, not a silent clobber.

`--depth 1` was tried and removed. It makes the clone cheap and the next refresh
wrong: a shallow fetch grafts the new commit into a root, `--ff-only` can no
longer see that the old commit is its ancestor, and every refresh fails with
"diverging branches". Two files this size do not repay that.

Two URLs, https first. Not a preference -- that is the order of who has to hold
a key: https needs nothing, ssh needs one. The reverse order was rejected
because `git@github.com` answering for a stranger's machine is a hang followed
by a timeout, and the first clone a machine ever makes should not take a minute
to say "no".

`.git` absent but the directory not empty is `Sync::Foreign`: something that is
not a clone lives there, and writing into it would be deleting a reader's files
to install files we can re-clone anywhere else. It stays as it is and the log
says so.

## start

Once per process (`STARTED`), then hourly, always off the caller's thread. The
first tick is immediate because a fresh machine has no community layer at all
and the sooner it lands the less of the run reads a half roster.

Nothing waits on it. The roster read is what a turn depends on, and a turn must
not wait for a network: `providers::roster` re-reads when a file it did not have
appears, so a clone that finishes during the run is picked up by the next
`set_session` with no restart and no coordination between the two modules.

The hour is wall-clock, not per read: this is content, it changes when somebody
commits, and a fetch per request would turn every turn into a DNS lookup.

## What is not owned here

The scripts. Nothing in this server runs `providers/*.py` today -- the roster names
them for whoever does, and `quota_query.script` resolves against the directory
of the roster file that named it. Cloning them is the whole of this module's
part; making them run would be a feature, not a plumbing detail, and one that
belongs in `SPEC.md` before it belongs in a `spawn`.
