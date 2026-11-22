# `crates/server/src/ship.rs`

## module doc

The harness ships three kinds of thing a user should be able to edit: the config, the
skills that speak about the server, and the Python surface a skill imports. None of them
belongs in this repository, because a file in the working tree is a file a project's own
edits and the harness's updates fight over. They live in a content repository, which a home
keeps as a checkout and **reads in place**: there is no install step, no copy beside the
checkout, and therefore no base, no merge and no conflict to converge.

That is the whole design. A home's config is `<checkout>/<declared config>`, its own skills
are `<checkout>/<declared skills>/<name>`, and `GONGGA_SKILL_LIB` names
`<checkout>/<declared python>`. An edit in the checkout is an edit here.

## MANIFEST, link

`ship.toml` is compiled in, and it names no commit: the newest one that speaks `protocol` is
read, so content moves by a restart rather than by a rebuild. `protocol` is the one number
that can force a rebuild, so it moves only when the *shape* of the content changes. A
manifest that does not parse is a build bug, so `link()` panics instead of degrading into a
server that reads nothing.

Two answers, most specific first: `GONGGA_SHIP_URL` (one run: a test, a shell) and the
compiled URL. A home may name its own repository too, in `[ship] url`; that one cannot be an
answer here, because the config that holds it lives *inside* the checkout the URL would have
to name first. It is applied at the fetch instead (`follow`), and a change to it lands on the
same start: point `origin`, fetch, `refresh`.

## Roots, checkout

Two roots, because the two kinds of content have different lifetimes. This repository owns
the config, the skills that describe *this server* and the Python surface: they are the
checkout in the home (`<home>/ship`). The machine's skills -- `proxy`, `remote`,
`computer_use`, `web-search` -- are the community clone's, which `agents::link_skills` puts in
`~/.agents/skills`. Installing those into a home would give one machine five copies of the
same procedure, so they are not here at all.

The checkout is kept the way the community roster is (`fork`): official `main`, personal
`local`, rebased. `ship.git` was the old bare cache, and `adopt` turns one into the other
without a network.

## Declared, declared_at

What the content says about itself, read from the commit it is read from: the protocol it
speaks, which path is the config, where the skills live, which path is the Python surface,
which paths an earlier commit left on the machine. It is parsed rather than copied into this
binary because a mapping is a fact about *that* repository's history: moving a skill or
retiring a path must not need a rebuild.

`commands` is the one entry that places nothing anywhere: it names the functions this content
publishes as commands of the home's own (see `server-content-commands.md`).

## resolve, settle

`resolve` reads the checkout's tip as it is and names the newest commit, walking first
parents *back* until one speaks this build's protocol. Walking back rather than forward is
the invariant: content that has moved to a protocol this build cannot read still serves this
build the last commit it could.

`settle` is what makes that commit the one on disk, because nothing is copied: the files a
home reads are the checkout's own working tree, so the tree has to *be* the content this
build reads. In the normal start it already is, and `settle` runs no git at all. When the
walk-back named an older commit, git moves the tree there -- onto the branch that names that
commit, or detached when no branch does -- and logs it. Work in the tree that would have to
go is a refusal: it is the reader's, and discarding it is not the server's to do.

## prepare, inspect, follow, refresh

`prepare` is the start's first half, before any config is read: adopt the old cache, clone if
the home has none, resolve, settle, check the three paths, and tidy what an older build left.
A checkout missing a declared path is refused, naming it: the home is no longer
self-contained, and a server that started anyway would have no config to say where anything
esle is.

`inspect` is the same question asked without writing anything -- no clone, no fetch, no
checkout. It is what `--check-config` runs before a handover cancels the server whose config
it is asking about: a refusal there costs a home its server, and a cancel cannot be taken
back.

`follow` points `origin` at the repository this home names and starts the one fetch the
start makes. `refresh`, behind the port, waits for that fetch, resolves again, settles again,
and replaces the live answer; a moved checkout therefore reaches a running server without a
restart. `ship::content` is a `RwLock`, so one call gets one `Content` and no reader holds a
reference into state another thread may replace.

`prepare` runs before `follow`, so a checkout whose history holds nothing this build can
read has no fetch to wait for -- and that is exactly the home a protocol step strands:
every attempt refuses, and the fetch it made dies with the process that refused. So
`wait_for_fetch` starts the fetch itself when no caller has (`start_fetch`, the same one
`follow` uses: one round trip per process, shared through `fetched`). The repository is
the one this home's config names, read at `config/config.toml` -- the name this mapping
has always had -- because the mapping that might name another file lives inside the
content this resolve cannot read, and asking for it sends the reader back here. This is
the only start that waits on a network; a checkout that answers is read as it stands, and
`refresh` moves it behind the port as before. Measured with protocol 15: a home whose
checkout still spoke 14 fetched, rebased `local` onto it, and listened in one start.

## Paths, and what reads them

`paths_of` is the only place the mapping becomes paths, and it refuses a declared path that
climbs out of the checkout (`safe_relpath`) or that is not there. `skills(home)`, `python(home)`
and `config(home)` are the same answer for a caller that holds only the home:
`tools::skill::roots` for the harness bucket, `GONGGA_SKILL_LIB` and the `gongga-cli` shim for
the Python surface, and `config::load_with_home` for the config.

## tidy

An older build installed copies: `<home>/config.toml`, `<home>/skills/<name>`,
`<home>/lib/*.py`, and the `.ship-base/` it merged them against. They are no longer read, and
`tidy` takes them away on every start. Its rule is bytes, because provenance is not on disk:

| path | goes when | stays when |
| --- | --- | --- |
| `<home>/config.toml` | its bytes are the content's config | it differs: kept, and named in the log |
| `<home>/skills/<name>` | `<name>` is a skill the content ships and every file in it is that skill's | any file differs, or the content never shipped that name (never looked at) |
| `<home>/lib/<file>` | the content's `python` tree has that name and the bytes are its | it differs, or the name is not the content's |
| `<home>/.ship-base`, `<home>/.ship-pin` | always: a record of a merge that no longer happens | -- |
| `<machine>/<retired path>` | always: the content dropped a path an earlier commit installed there | -- |

A copy that differs -- an older protocol's, or one somebody edited -- stays exactly where it
is and is named in the log. That is a cleanup, not a decision about whose work a file is, and
a stale copy is no longer read by anything.

`retired_python::retire` runs first, for the three names an older *build* wrote into
`<home>/lib` from its own bytes: those are not the content's bytes, so only that module can
recognise them.

## safe_relpath

A repository path becomes a path under the checkout, and a path like `../../server.key` would
otherwise write outside it. Refusing on `..`, on an absolute path and on a drive letter, and
accepting only `Normal` components, is cheaper than trusting the content repository.

## Report

There is none any more. What a start has to say is one line -- the commit, the config, the
skills and the Python surface it reads -- plus a line per file `tidy` took away and a line
per copy it left because it was not the content's.
