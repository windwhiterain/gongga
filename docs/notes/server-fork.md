# `crates/server/src/fork.rs`

## module doc

Two repositories are read this way: the community roster (`agents`) and the shipped
content (`ship`). Both are official-upstream plus a machine's own commits, so the rule
is one file: fetch, fast-forward the official branch, rebase the personal branch onto
it, and never leave a half-finished rebase behind.

## OFFICIAL, PERSONAL

The branch names are defined here rather than in each caller, because two names for
one branch is how a pin drifts: `scripts/ship.sh` reads them out of this file, so the
publisher and the reader cannot disagree about which branch is. `local` has no
upstream by design — its commits are not meant to be pushed anywhere, and upstream
lands on it only by rebase.

## Fork::ensure, Fork::sync

`ensure` clones when the directory is absent and leaves anything else alone: a
directory this module did not clone is somebody else's, and both roots are read by the
server, not owned by it. `sync` is best effort by construction — a machine with no
route to the repository keeps what it has, and the caller decides what an unreadable
root means for it (`agents` warns, `ship` refuses to start only when it has nothing).

## fast_forward_official

The official branch moves without being checked out: `fetch . origin/main:main` moves
the ref and leaves the worktree alone, because a refresh in the background must not
move what a window is showing. A worktree already on that branch needs the ordinary
`merge --ff-only` instead — a refspec cannot move a branch that is checked out. A
non-fast-forward means somebody committed to the local official branch, and it is left
alone: overwriting a person's branch is worse than a stale one.

## rebase_personal

Only when the personal branch is the one checked out, for the same reason. A conflict
is undone, not left behind: a half-finished rebase blocks the next refresh and
confuses anyone typing `git status`. The branch keeps the commits it had, the conflict
is reported, and the next refresh tries again — a person resolves it when they are
ready, never the server.

## Fork::content, Fork::blob

`content` is the commit a root's content *is*: the personal branch when there is one,
the official branch otherwise, and HEAD as the last answer. `blob` reads one file at
one commit through `git show`, which is how content that is not checked out is read —
and it is what makes the protocol walk (`ship`) possible without a worktree.

## git, git_ok, bounded, program

`git -C <dir>` so one checkout is asked about instead of the process's directory.
`bounded` is git bounded by its own silence rather than by duration: a stalled transfer
and a credential prompt are what hang a start, and a slow transfer is still allowed to
finish. `GIT_TERMINAL_PROMPT=0` and `BatchMode=yes` are the other half of that — a
prompt has no console to appear on, and it would wait forever for one.
