# git workflow

Three checkouts, one direction each: the source repository this build is made from, the
content a home reads, and the community roster. Two of them are kept the `fork` way
([`server-fork.md`](server-fork.md)); the third is never published at all.

| checkout | where | branches | publishes by | protocol |
| --- | --- | --- | --- | --- |
| source | `C:/resource/gongga`, origin `gongga.git` | `main`, and `<prefix>/<name>` inside a worktree | nothing (`publishes=0`) | reads `crates/server/ship.toml` |
| content | `ship/`, the submodule of this repository, and a checkout of its own in every home, origin `gongga-ship.git` | `main` + `local` | `scripts/publish.sh content -m` | says `ship/ship.toml` |
| agents | `C:/Users/17124/.agents/gongga-agents`, origin `gongga-agents.git` | `main` + `local` | `scripts/publish.sh agents -m` | none |

The three names, and everything that differs between them -- where the checkout is,
which remote it goes to, whether it declares a protocol, whether it may be published at
all -- are `repo_config` in `scripts/publish.sh`, and no other line there knows a name.

## `main` is official, `local` is this machine

`main` holds commits that belong to everyone; `local` holds the ones that belong to this
machine only. That is the whole split, and it decides every operation: `main` moves by
fast-forward from the remote and by a checkpoint merging a feature branch into it, never
by editing a file on it, and `local` is rebased onto `main` and never pushed. `local` has no upstream **by design** (`crates/server/src/fork.rs`),
so no `git pull` can quietly merge the official line into it: the only way `local` meets
`main` is the rebase publishing does.

The branch names are defined in `fork.rs` rather than typed at each caller, because two
spellings for one branch is how a pin drifts; the publisher and the server that reads
the content therefore cannot disagree about which branch is which.

## The source repository publishes nothing

Its `main` is this machine's line: being ahead of `origin/main` is the expected state,
not a mistake to repair, and a checkpoint merges the feature branch into it so parallel
sessions stay in step. `publish.sh source` exists only so that one command can read all
three checkouts; its copy has `publishes=0`, and that is the statement of the rule.

## Publishing is commit, rebase, push -- in one call

    scripts/publish.sh <repo> -m "..."         # commit, rebase onto origin/<official>, push
    scripts/publish.sh <repo> --check          # agreement, and publication
    scripts/publish.sh <repo> --check --local  # agreement only, no network
    scripts/publish.sh <repo> --dry-run -m "..."
    scripts/publish.sh <repo> --where          # the configuration a name resolves to
    scripts/publish.sh <repo> --state          # what that checkout is, read-only

`scripts/ship.sh` is the content's front end: same flags, same exit codes, with
`content` already chosen. The exit codes are a contract and are read as one: `0` agreed
and published, `1` a real disagreement (protocol drift, a branch that may not publish, a
push that failed), `2` usage, `3` the remote did not answer -- which is not a
disagreement, because whether it is published is still open.

A rebase that conflicts undoes the whole attempt and pushes nothing: the branch keeps
the commits it had and a person resolves it. Nothing here publishes a half-merged state,
and nothing here publishes a message nobody wrote -- `-m` is the release note, not a
default.

## Two protocol numbers, and the order they move in

`ship/ship.toml` is the protocol the **content** says it speaks; `crates/server/ship.toml`
is the one this **build** reads. `--check` compares them, and only for the content: a
roster is prose a server reads, not a shape it parses, so the agents repository has none.

When the content's shape really changes -- a new required field, a new layout -- only the
content is cut over first, and a build that understands it follows. Old builds are not
stranded: they walk first-parent back through the content's history and install the last
commit whose protocol they can read, which is why content keeps no pin and a publish
produces no commit in this repository. A prose fix, a new skill, a retired path is
content and moves no number.

## Hooks, and what they can and cannot guard

`git config core.hooksPath scripts/hooks`, once per clone: the hook lives in the tree
(`scripts/hooks/pre-commit`) rather than in one checkout's `.git/hooks`, so every
worktree runs the same one.

It refuses four things, each when the commit is one that could carry it: an `origin`
that names the content repository (the mistake it was written for -- see
[`server-fork.md`](server-fork.md)), a skill body spelled with this harness's own
install path, a tool this build carries without its words in the shipped config, and --
only when the commit touches `ship` or `crates/server/ship.toml` -- `ship.sh --check
--local`, the half of `--check` that reads two manifests and no network, because a
commit is not allowed to depend on one.

A hook is a promise about commits made here. It cannot see `--no-verify`, a clone that
never installed it, or any push made by something other than this git; the receiving
side is what covers those -- no force-push and no branch deletion on the content
repository's `main`.

## Checking the shape, which is not publishing

    scripts/git_check.sh     # 0 every checkout is allowed, 1 at least one is not

Read-only: nothing is fetched, nothing is written, no remote is asked. One line per
checkout -- branch, ahead/behind, hooks, dirty -- then `PASS` or `FAIL`. It allows the
source on `main` or on a named `<prefix>/<name>` branch, and the content and agents on
exactly `main` or `local`; it reports a detached content checkout with nothing in it as
`pristine` (what `git submodule update --init` leaves). What fails is a branch this flow
does not allow, a conflict left in the tree, hooks not installed where they are needed,
and an upstream that names another of the three repositories.

That last one is the failure worth keeping: it is the shape of the mistake that pointed
the source repository's `origin` at the content URL. This is **not** `publish.sh
--check`, which answers "is what I would publish published" -- false for any work in
progress. This asks whether publishing is the next step.

## A home reads its checkout; editing it is a commit on `local`

A home keeps no second copy: it reads `<home>/ship` in place, so changing this machine's
config or a skill is a commit on the content's `local` branch and a publish, not an edit
to the home. The reasons, and the boundary of what is live-tracked, are in
`.agents/notes/2026-11-14-a-home-reads-the-checkout.md`.

## Worktrees

Every session works in a worktree of its own under `./.worktrees`, one feature branch per
piece of work, and a checkpoint merges that branch into `main` so parallel sessions stay
in step; the branch is deleted after the final merge. Two consequences are not optional:
a worktree gets its own `CARGO_TARGET_DIR` -- a shared one makes concurrent cargo queue on
the lock, and on Windows a running `.exe` is held open, which a relink reports as
`Access is denied (os error 5)` -- and a worktree shares the main checkout's config, so a
misconfigured `origin` there is misconfigured in every worktree at once.

## Forbidden, and why

Never force-push, never `--no-verify`, never leave a hook undone, never commit on `main`
directly, and never ask git a question with `git -C`.

The last one is not style. `git -C <dir>` answers for the repository the path sits
*inside*, so a `.git` that git cannot read -- an interrupted clone leaves exactly that --
makes the command read and write the **enclosing** repository's config. A test home
lives inside a worktree, and a worktree shares the main repository's config, so this is
how `origin` twice became the content's URL. Every git call this project makes names
`--git-dir` and `--work-tree`, and decides ownership by the **common** git directory.
See [`server-fork.md`](server-fork.md).
