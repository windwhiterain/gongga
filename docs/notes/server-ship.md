# `crates/server/src/ship.rs`

## module doc

The harness ships three kinds of thing a user should be able to edit: the config,
the skills that speak about the server, and the skills that speak about the
machine. None of them belongs in this repository, because a file in the working
tree is a file a project's own edits and the harness's updates fight over. They
live in a content repository of their own, which a home keeps as a checkout and
installs into itself.

## MANIFEST, link

`ship.toml` is compiled in, and it names no commit. A pin meant a content edit cost a
commit here plus a rebuild, and the two names for one commit drifted the moment
somebody forgot the second half; instead the URL and the protocol this build reads are
what the binary carries, and the commit is whatever the branch says today.

`protocol` is the one number that can force a rebuild, so it moves only when the
*shape* of the content changes: a new required key in the content's declaration, a
different layout. A wording fix, a new skill or a retired path is content, and content
never needs a new build.

A manifest that does not parse is a build bug, so `link()` panics instead of degrading
into a server that installs nothing.

## which repository, and whose answer it is

Three answers, most specific first, and one of them is a fact about a home rather than
about a build:

| written where | who it is for | how it is spelled |
| --- | --- | --- |
| the environment | one run: a test, a shell, a throwaway repository | `GONGGA_SHIP_URL` |
| this home's config | a machine developing the content | `[ship] url = "C:/work/gongga-ship"` |
| this build | everybody else | `[ship] url` in `crates/server/ship.toml` |

The middle one is what a developer needs and the environment cannot give: the URL
belongs to the *home*, so a window started tomorrow installs from the same place, and
two homes on one machine may install from two checkouts. `adopt` follows the change --
it re-points the home's `origin` -- so switching a home between the published content
and a local checkout is a restart, and switching back is the same edit removed.

A local path costs nothing to fetch and needs no push: the working tree a person edits
*is* the remote. What it does not change is which commit is installed: the protocol
still decides that, and a local `main` that has not moved still installs what it said
yesterday. In practice a development loop advances `main` in the local content
repository (`git -C ship merge --ff-only local`) and the next start installs it --
one local fetch, no network.

One home per process, and one URL: the resolve is memoized (`CONTENT`), so the first
answer is the server's answer. `remember_url` is therefore called from `lib.rs`
immediately after the config is read, before `prepare` can resolve anything -- a URL
arriving later would be a second content for one server, which is the shape of bug
that shows up as "some sessions read the new skill and some the old".

## Roots, machine_root, checkout

Two roots, because the two kinds of content have different lifetimes. What this
repository owns -- the config, and the skills that describe *this server*
(`handoff`, `image`, `self-control`, `subagent`) -- belongs beside the database and
follows the home. The machine's skills -- `proxy`, `remote`, `computer_use`,
`web-search`, `latency` -- are the community clone's, which `agents::link_skills`
puts in `~/.agents/skills` beside `providers.toml`. This repository does not ship
them: installing them into a home would give one machine five copies of the same
procedure and five chances to edit them apart, and installing them into `~/.agents`
would fight the clone for the same names.

A home keeps its copy at `<home>/ship`: a checkout, kept the way the community roster
is (`fork`) — official `main`, personal `local`, rebased. A person can therefore edit
what this machine installs, and the edit survives an upstream change. `ship.git` was
the old bare cache, and `adopt` turns one into the other without a network.

## Declared, declared_at

What the content says about itself, read from the commit it is installed from: the
protocol it speaks, which path is the config, where the skills live, which paths an
earlier commit left on the machine. It is parsed rather than copied into this binary
because a mapping is a fact about *that* repository's history — moving a skill or
retiring a path must not need a rebuild, which is the whole point of resolving by
protocol.

## plans

`config.toml` is mapped from one repository file to one home file; a skill bucket is
mapped from a repository *tree* spread under `skills/`. The difference matters for
`collect`, which yields one entry per file either way, and for what gets removed when
upstream drops a path. The tag on each entry names that root's copy of the merge base.

## reconcile

Three versions per file: the disk (ours), the content (theirs), and the bytes this
root last installed (the base). The base is why a content edit lands on a file nobody
touched and does *not* land on one somebody did: without it, "differs from upstream"
and "the reader edited this" look identical.

The base lives in `<home>/.ship-base/<root>` rather than in git history, because the
personal branch is rebased and a rewritten commit is no longer a base. It is a copy of
what was placed, so it also says exactly what the last install did rather than what
some commit said.

`Report` is shared by both roots, so this root counts only what it added before
deciding whether to advance its pin. Without that, one kept file in the machine root
would freeze the home root's pin and repeat a merge that already resolved.

A conflict or a kept file leaves the pin where it was, so the next start repeats the
same three-way merge against the same base. Advancing the pin past an unresolved file
would silently adopt the unmerged result as the new truth.

## seed_base, remember

`seed_base` copies the old pin's tree into the base directory, once, for a home
installed by an older build that recorded a pin and no base of its own. It happens
only when no base exists: a base that is there is what the last install placed, and
re-seeding it from a commit would silently adopt that commit as the truth for every
local edit.

`remember` is the other half — every file placed is recorded, which is what makes the
next merge three-way rather than a guess.

## Decision

Six outcomes, and the sixth is the reason the tool exists: `Keep` is a file the reader
authored where the content also has one. What `decide` sees is ours, base and theirs,
and nothing else: "these bytes are the last install" is already computed, because the
base is a copy rather than a commit lookup.

`Adopt` exists for the other side: a file already byte-identical to the content counts
as current, not written, so a restart logs `written=0` and the log line means "this
start changed nothing".

## merge_text

`git merge-file -p` with the base as `--local`'s neighbour: three real files in a
temporary directory and stdout as the answer. Doing the merge by hand would mean
shipping a diff3 implementation for the one operation git already does well and that
users already know how to resolve. `-p` keeps the base file untouched, which matters
because the base is on disk only as a temporary convenience.

## collect, listed, blob, config_text

Paths come from `ls-tree -r --long`, so a shipped file's mode is known before it is
written -- a skill script has to stay executable, and a `config.toml` that is not.
Content comes from `git show <commit>:<path>`. `config_text` is the same read for one
path, and it is what makes the resolved commit the source of the prompt defaults:
nothing copies that prose into this binary.

## safe_relpath

A repository path becomes a path under a root, and a path like `../../server.key`
would otherwise write outside it. Refusing on `..`, on an absolute path and on a
drive letter, and accepting only `Normal` components, is cheaper than trusting the
content repository, which is a third party by definition.

## resolve, newest_for, protocol_at

`resolve` is the whole answer for one start, kept in a `OnceLock`: the migrator, the
install and the prompt defaults all read one commit, so a fetch landing mid-start
cannot make them disagree about what "the content" is.

`newest_for` walks back through first parents until it finds a commit that speaks this
build's protocol. Walking *back* rather than forward is the invariant: content that
has moved to a protocol this build cannot read still serves this build the last commit
it could read. The alternative is a machine that cannot start because somebody else's
repository advanced — and a build whose content has no such commit refuses, naming
both numbers.

## Report

The counters exist for the log line; the lists exist because they name files a human has
to open. `written` and `updated` on a start that should have changed nothing is the signal
that the content moved under a home. `reachable` says whether the repository answered
this start, which is the one thing a log line has to distinguish from "nothing changed".
