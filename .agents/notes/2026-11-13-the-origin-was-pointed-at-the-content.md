# The main repo's origin was pointed at the content repo (2026-11-13)

## What the user asked
"An agent pointed the main repo's git remote at the ship repo; find out why, and how to
avoid it."

## Why: `git -C` answers for the repository *above* the path

Not a hand-typed URL. The evidence is a server log from a test home, plus the shape of
the directory it was reading:

```
.dev/live/20261113-175510/server.log
  INFO gongga_server::ship: content origin follows this home's config
       home=C:\resource\gongga\.dev\live-home
       from=https://github.com/windwhiterain/gongga.git      <- this repository's own origin
       to=https://github.com/windwhiterain/gongga-ship.git
.dev/live-home/ship/.git                                    <- an EMPTY directory
$ git -C .dev/live-home/ship rev-parse --show-toplevel
C:/resource/gongga
```

`ship::point` then ran `git remote set-url origin <content url>` in that directory, and
`git -C` applied it to the repository git answered for: this one. A test home lives
*inside* the worktree (AGENTS.md asks for that), so a stale `<home>/ship` is inside
this repository, and `Fork::exists` only asked whether a `.git` entry was there -- which
an emptied clone satisfies while git refuses to read it. The same log line appears with
`from=git@github.com:windwhiterain/gongga.git` on 2026-11-11, so it has happened at
least twice, on two different spellings of this repo's origin.

The state I inherited was `origin.url = gongga-ship.git` with `pushurl = gongga.git`:
`set-url` writes `url` alone, which is what makes it look like a person's edit.

## The fix

Ask git, never a `.git` entry:

- `Fork::exists` (`crates/server/src/fork.rs`) is now `rev-parse --show-toplevel`
  resolved and compared with the checkout's own path, so "a clone is here" means *this
  path is its root*. Every writer -- `ship::point`, `ship::adopt`, `migrate::blob` -- is
  guarded by it, and a foreign directory now stops the start with "is not a clone, so it
  is not the shipped content" instead of editing somebody's config.
- `ship::adopt` asks with an explicit `--git-dir`, because the cache it adopts is
  *bare* and has no worktree to ask about, and then verifies the converted checkout with
  the same question.
- `scripts/ship.sh` and `scripts/tui_live.py`'s seeding ask the same question before they
  write: the harness used to report "seeded" for a directory git was answering for from
  above, which is how the poisoned home kept being reused.
- `scripts/hooks/pre-push` is new: `pre-commit` already refuses a commit with such an
  origin, and a push is the other moment a wrong remote costs something.

## Verification (reproducible)

- The poison shape, reproduced: `.dev/verify-poison/ship/.git` empty, home inside the
  worktree. `git -C .dev/verify-poison/ship rev-parse --show-toplevel` says
  `C:/resource/gongga`; starting the new server on that home prints
  `Error: ...\ship is not a clone, so it is not the shipped content`, no "content origin
  follows" line, and `git config remote.origin.url` is unchanged before and after.
- `git push` to a fake `.../gongga-ship-fake.git` remote: refused by the hook, exit 1,
  nothing pushed.
- `bash scripts/ship_selftest.sh`: `PASS · 77s · ship-protocol` (its first run caught a
  regression in my own `adopt` guard -- a bare cache has no `--show-toplevel` -- which is
  why the guard names the git dir explicitly).
- `cargo build`, `cargo fmt --check`, `comments-check` on fork.rs/ship.rs back at their
  pre-existing counts (2 / 1), `test-layout-check`, `skill_contract.sh`.

## Follow-up (B): git is given its directories

User chose B: the server must stop letting git *search*. Every command now names
`--git-dir` and `--work-tree` (`fork::git_at`), so nothing can be answered for by an
enclosing repository; `fork::git` refuses a directory that is not a checkout by naming
`<dir>/.git`, which git then refuses. A path is a checkout only if its own `.git` names
a git directory whose **common** directory is that `.git` (`fork::owned`) -- which is
strictly stronger than the `--show-toplevel` test I first wrote: a *worktree* of this
project has a toplevel of its own and still shares this repo's config, so the first
test would have let `remote set-url` rename this repo's origin through `<home>/ship` as
a worktree. Proven: with `ship/` a worktree of this repo, the new server logs no
"content origin follows", exits "is not a clone", and `remote.origin.url` is
unchanged. (My first run of that probe corrupted the origin again -- the binary was
built from a *stashed* baseline, because `git stash` in this repo is shared across 36
worktrees and I stashed while measuring warnings. Repaired with `set-url`; the fix was
rebuilt and re-probed.)

**`git stash` is shared by every worktree of this repository** -- one more shared-state
trap of the same family. Another session's WIP (`crates/server/src/{session,tools/*}.rs`)
is in this working tree and was swept by that stash; it was restored untouched and is
not part of this commit.

## Still open
The tree records no URL for this repository, so a hook can only refuse the *content*
URL, not verify the right one. Recording the upstream somewhere (and having the hook
compare exactly) would catch the SSH/HTTPS spelling change too; that is a decision
about where the project's own remote is written down, so it is the user's.
