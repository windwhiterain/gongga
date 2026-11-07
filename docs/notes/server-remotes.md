# `crates/server/src/remotes.rs`

The machines this server may run a session's shells on, and where that list is kept.

## The file is the machine's, not the home's

    ~/.agents/remotes.toml        # $GONGGA_AGENTS_HOME, or $GONGGA_REMOTES_CATALOG

A remote is made of three facts about this machine: a `host`, an `identity` — the path
of a private key — and a `runner`, a path on the peer that changes with every build.
None of them belongs in `<home>/config.toml`, which is the **pinned content's**: the
install pass merges it three ways on every pin move (`ship.rs`), so a value written
there is a value that can be published, merged into somebody else's file, and carried
to a machine where the key it names does not exist.

The home already draws this line elsewhere, and this file joins it:

| file | whose |
| --- | --- |
| `<home>/config.toml` | the pinned content's, merged on every install |
| `<home>/server.key` | this machine's credential |
| `<home>/client.toml` | the client's own state |
| `<home>/gongga.db` | this machine's data |
| `~/.agents/remotes.toml` | this machine's peers |

`~/.agents/` is not a repository and never becomes one — that is the same reason
`keys.toml` lives there. See `docs/notes/server-providers.md`.

## `path`, `read`, `stranded`

`path` is the override, then the machine's own directory. `read` is one file: every
`[remotes.<name>]` table, in file order, each judged by `FileRemote::into_config`.

What a table cannot say is how long its peer may stay silent: that number is the
home's, `[remote] answer_timeout_ms` in `config.toml`, and it is handed to every table
this function loads. It bounds the link and not a call, so it is one number for every
peer rather than a field an operator repeats. See `docs/notes/server-remote.md`.

A table this build cannot use is **absent, with a line saying why** — never a fallback
to this machine. An unknown name is already refused by `SessionManager::endpoint_for`,
and the pair together is the invariant: a session that asked for a peer never quietly
gets a local shell.

`[targets.<name>]` is aliased: a table pasted from an old `config.toml` still reads
rather than vanishing in silence. The migrator rewrites the header, so a file converges
on one spelling.

## `stranded`: the one thing that must not be silent

A home whose `config.toml` still names remotes has remotes this build does not have.
Nothing would read them, and the only symptom would be `no remote named "lichen-vm"` —
a sentence that says nothing about the file the name is sitting in. So startup reads
both spellings out of `config.toml` and warns with the names and the command that moves
them (`migrate/3-to-4.py`).

## A change needs a restart

The list is read once, at startup, like `config.toml` and unlike the roster: adding or
editing a remote is a restart, and in development that is the `evolve` skill. Making it
hot would mean a lock around every `endpoint_for`, which is a different change and not
this one.

## What a name is, and is not

The name is the table's own header. It is the operator's word: the model never sees it,
because the tools say `machine: local | remote` — see `docs/notes/server-tools-mod.md`.
A stored session keeps the name and never the host, so `remote.py` may rewrite the
table freely between two runs.
