# server/worktree.rs

SPEC's `Workspace.worktree`: the branch and the directory that come into being with a
session and go away with it.

## One name, both things

`create` makes them together: a branch named exactly what the caller stated, and a
worktree of that name at `<the repository's first worktree>/.worktrees/<name>`. That is
where this project keeps its own (see `AGENTS.md`, "Worktree"), and a name with a slash
(`feature/x`) makes `.worktrees/feature/x` -- git creates the parents.

The branch is drawn from the repository's first worktree, the entry
`git worktree list --porcelain` prints first, never from wherever the asking directory
happens to be. A session created inside a linked worktree therefore starts on the same
commit as one created in the main checkout, and its new worktree lands beside the
repository's own rather than beside the caller's.

## Which repository, and on which machine

Every git call runs on the session's own [`Space`](server-space.md), standing in the
session's directory: this machine for a local session, the peer for a remote one. There
is no second implementation, so a remote session's worktree is made by the git that
machine has, and the path recorded is that machine's spelling.

The repository is whatever git answers for from that directory -- the nearest checkout
at or above it. This is asked deliberately, and it is the one place in the server where
"which repository does this directory sit in" is the question being asked, so it is git
that answers it (nothing writes a config here; `server-fork.md`'s `--git-dir` rule exists
because a config write once landed in the enclosing repository). A `cwd` inside no
checkout is refused in git's own words.

`Workspace.cwd` still says where to look. A session born in a worktree works at the
worktree's root: the stated directory named the checkout the branch was made in, not a
subdirectory of the new tree. A peer's directory is passed as written; an unstated one
is that machine's own starting directory.

## One session per name

Two collisions are refused before anything is made, each in the words of what is already
there: a branch of that name, whether it is out in some worktree or merely exists, and
anything already at the target directory. Git's own `check-ref-format --branch` decides
whether a name is a name at all, so the directory that follows from it cannot be one git
would refuse.

The worktree is made with a session and never adopted by one: `set_session` on an
existing session refuses a worktree it does not already record, and a session that owns
one is not moved -- neither to another directory nor to another machine. A worktree is a
place, and the session that asked for it stays in it; moving away would leave the branch
behind with nobody to remove it.

## Dying with its session

`del_session` takes the worktree away before it stops anything, so a git that will not
let go -- a peer that is not answering included -- leaves the session exactly as it was.
Forced: the caller said the session is gone, so whatever was uncommitted in that
directory goes too. The branch is deleted after the directory, and a refusal from either
is the answer rather than a warning -- what is left otherwise is a branch no session will
ever remove.

Administration runs in the repository's first worktree, never inside the one being taken
away. Being *in* the directory is enough to hold it open: git then unregisters the
worktree, fails to delete the directory (`failed to delete ...: Permission denied`, on
Windows), and leaves a directory nothing lists any more. Measured, and the reason the
removal has a working directory of its own.

A `cwd` that sits in no repository means the worktree went without git. The registration
is pruned when git still lists it, the branch is deleted when it is still there, and the
answer says the worktree was already gone. A directory that is not registered as that
branch's worktree is left alone: nothing here removes a directory git does not claim.

## Stored, not derived

The row's `worktree` column holds the name and its `cwd` the directory. `clone_session`
copies neither as an ownership: the copy works in the same place and owns no branch,
because a branch has one session that made it (`server-store.md`).
