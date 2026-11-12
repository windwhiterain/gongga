# server/agents.rs

## agents

The machine's shared content, kept in a clone under `~/.agents`. Today that is
the roster and the quota scripts beside it; the module is named for the
directory rather than for the two files because the next thing to live there
will not be a roster, and a module renamed every time is a module nobody cites.

## The refresh cannot hang

`sync` runs on the blocking pool, which the runtime joins at the exit, so a `git
pull` that never gives up would hold the exit for as long as the kernel keeps the
connection -- the same failure as a wait on a window that never closes. Git is
therefore told what a stalled transfer is: below a trickle for longer than the
limit it aborts (`http.lowSpeedLimit` / `http.lowSpeedTime`), an ssh transport is
asked not to prompt and to give up on a connection in ten seconds, and
`GIT_TERMINAL_PROMPT=0` keeps a credential question from becoming a hang with no
console to appear on.

This bounds silence, not duration: a slow but moving transfer is still allowed to
finish, because AGENTS.md forbids turning a wait into a budget.

## Roots

Two directories, and the shape inside them is the same, so a relative path means
the same thing in both:

| root | who writes it | what it is |
|---|---|---|
| `~/.agents/gongga-agents` | gongga, by cloning | the community repository |
| `~/.agents` | the user | this machine's own files |

The sameness is deliberate. `quota_query.script` in the roster is a path
relative to the file that names it (`providers/deepseek.py`), so it resolves
inside the clone with no table of "where scripts live" to keep in sync. A layout
that buried the scripts one level deeper would have needed that table for exactly
one level of nesting and bought nothing.

The roster is **one** file under that root, not one per root: a machine's own
entries are a branch of the checkout (`local`), so an upstream change is a rebase
and there is no overlay to merge on every read. See
`docs/notes/server-providers.md`.

## The refresh, branch by branch

`git pull --ff-only` was the wrong verb: it needs the branch that is checked out to
name an upstream, and the branch this machine reads has none on purpose -- its
commits are not meant to be pushed. So the refresh does the three steps a person
would:

1. `fetch origin`;
2. move `main` to `origin/main` **without touching the worktree** (`fetch .` into
   the ref), because a background refresh must not switch what a window shows;
3. put the machine's branch (`local`) back on top of `main`.

A conflict is undone with `rebase --abort` and reported. Leaving it in progress
would block the next refresh and make every `git status` in that checkout a
mystery; resolving it is a person's decision, and the commits are still there to
resolve against.

The rebase also runs only when `local` is the branch checked out. If a person is on
`main`, the refresh fast-forwards `main` and stops: moving the worktree out from
under them is exactly what a background task must not do.

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

## link_skills

A skill is only useful where the server looks, and that walk reads two roots
the home's `skills/` and the machine's. The community clone is not one of them,
so its skills are linked into the machine's directory instead of being taught to
the index as a third place -- one place to look is what makes "a name the reader
has wins" true without the index knowing about precedence at all.

A junction on Windows, a symlink elsewhere: a directory symlink there wants a
privilege the server does not have, and `mklink /J` wants none. Both are links a
reader can still see through, which is the point -- editing the skill in place
edits the clone, and the next `pull` decides what happens to the edit.

Never over an existing name: that directory is the reader's override, and a
skill they wrote is not the community's to replace. A link whose target is gone
is dropped, so a skill deleted upstream stops being listed.

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
