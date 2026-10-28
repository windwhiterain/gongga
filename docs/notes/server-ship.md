# `crates/server/src/ship.rs`

## module doc

The harness ships three kinds of thing a user should be able to edit: the config,
the skills that speak about the server, and the skills that speak about the
machine. None of them belongs in this repository, because a file in the working
tree is a file a project's own edits and the harness's updates fight over. They
live in a content repository of their own, and this module installs them into a
home from a commit this binary pins.

## MANIFEST, pinned

`ship.toml` is compiled in. Pinning by full sha rather than by branch means the
prose a build was tested with is the prose that build sends; a force-push to
`main` upstream cannot change what a running server says. A manifest that does not
parse is a build bug, so `pinned()` panics instead of degrading into a server that
installs nothing.

The `url` is any git URL, including a local path: `install_at` takes the pin and
the roots as arguments, so a test drives a throwaway repository and never touches
a real `~/.agents`.

## Roots, machine_root

Two roots, because the two buckets have different lifetimes. The harness bucket
speaks about *this server* -- `compact`, `evolve`, `image`, `self-control` -- so it
belongs beside the database and follows the home. The general bucket speaks about
*this machine* -- `proxy`, `remote`, `computer_use`, `web-search`, `latency` -- so
it lives in `~/.agents/skills`, beside `llm.toml`, which the server
also reads live. Installing the general bucket into a home would give one machine
five copies of the same procedure and five chances to edit them apart.

## install_at (refusal)

No network and no cache is the one case that stops startup: a home with nothing
installed and an unreachable URL would otherwise boot, take a turn, and answer with
an empty prompt. A cache that already holds the pin answers instead, with a warning,
because being offline is not being broken. See `docs/notes/server-prompts.md` for
what the prose is used for.

## plans

`config.toml` is mapped from one repository file to one home file; a skill bucket is
mapped from a repository *tree* spread under `skills/`. The difference matters for
`collect`, which yields one entry per file either way, and for what gets removed when
upstream drops a path.

## reconcile

Three versions per file: the disk (ours), the pinned content (theirs), and the
content of the pin this root was last reconciled against (the base). The base is why
an upstream edit lands on a file nobody touched and does *not* land on one somebody
did: without it, "differs from upstream" and "the reader edited this" look identical.

`Report` is shared by both roots, so this root counts only what it added before
deciding whether to advance its pin. Without that, one kept file in the machine root
would freeze the home root's pin and repeat a merge that already resolved.

A conflict or a kept file leaves the pin where it was, so the next start repeats the
same three-way merge against the same base. Advancing the pin past an unresolved file
would silently adopt the unmerged result as the new truth.

## Decision

Six outcomes, and the sixth is the reason the tool exists: `Keep` is a file the reader
authored where upstream also has one. It is not a merge failure -- nothing was
refused -- it is the answer to "should the harness overwrite the config I wrote?",
which is no.

`Adopt` exists for the same reason from the other side: a file already byte-identical
to the pin counts as current, not written, so a restart logs `written=0` and the log
line means "this start changed nothing".

## merge_text

`git merge-file -p` with the base as `--local`'s neighbour: three real files in a
temporary directory and stdout as the answer. Doing the merge by hand would mean
shipping a diff3 implementation for the one operation git already does well and that
users already know how to resolve. `-p` keeps the base file untouched, which matters
because the base is on disk only as a temporary convenience.

## collect, listed, blob, config_text

Paths come from `ls-tree -r --long`, so a shipped file's mode is known before it is
written -- a skill script has to stay executable, and a `config.toml` that is not.
Content comes from `cat-file blob`. `config_text` is the same read for one path, and
it is what makes the pin the source of the prompt defaults: nothing copies that prose
into this binary.

## safe_relpath

A repository path becomes a path under a root, and a path like `../../server.key`
would otherwise write outside it. Refusing on `..`, on an absolute path and on a
drive letter, and accepting only `Normal` components, is cheaper than trusting the
content repository, which is a third party by definition.

## fetch, has_commit

The cache is bare (`<home>/ship.git`) and holds only `refs/heads/ship/*`: a home
directory that contains a working tree invites someone to edit the thing the pin
reads. `+refs/heads/*` is the refspec, because a pin that moved to a rewritten branch
still has to be fetchable, and `has_commit` is the question that decides whether a
failed fetch is damage or merely offline.

## Report

The counters exist for the log line; the two lists exist because they name files a
human has to open. `written` and `updated` on a start that should have changed nothing
is the signal that the pin moved under a home.
