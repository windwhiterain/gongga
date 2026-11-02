# A remote belongs to the machine, not to the home

User's point, 2026-11-02: `config.toml` is content that goes into git and is merged on
every install, while a remote is local configuration. Put together, that means the
previous commit published the wrong thing: `[remotes.<name>]` carried a host name, the
**path of a private key**, and a runner path that changes with every build — in the one
file the install pass merges three ways (`ship.rs`), and taught the `remote` skill to
write it there.

## The line the home already had

| file | whose | merged |
| --- | --- | --- |
| `config.toml` | the pinned content's | yes, three ways |
| `server.key` | this machine's credential | no — and the file says so |
| `cli.toml` | the client's own state | no |
| `gongga.db` | this machine's data | no |

`~/.agents/` is where the other machine-facts already live (the roster, `keys.toml`,
the community clone), and it is not a repository — which is the same property
`keys.toml` is documented to rely on. So a remote goes beside them:

    ~/.agents/remotes.toml      # $GONGGA_AGENTS_HOME, or $GONGGA_REMOTES_CATALOG

Chosen over the alternative of a server command (`set_remote`) plus a database table:
that would need a new `Cmd`, would put a credential path in the database, and would
have to decide what `sessions.remote` means when the row it names is deleted. A file
beside the roster answers all three by not existing.

## What changed

- `crates/server/src/remotes.rs` (new): `path`, `read`, `load`, and `stranded` — the
  one thing that must not be silent. `RemoteConfig` moved here from `config.rs`.
- `config.rs` no longer builds the list; it still *parses* the tables, only to warn
  that this build does not read them, with the names and the migrator to run.
- `migrate/3-to-4.py`: moves a home's tables out of `config.toml` into the machine
  file, rewriting `[targets.x]` to `[remotes.x]`; a name already there that *disagrees*
  fails the step rather than picking one, and nothing is deleted from `config.toml`
  until every move has succeeded. `migrate::VERSION` is 4.
- `ship/skills/remote/remote.py`: writes `~/.agents/remotes.toml`, and its own flag is
  now `--remote` — one word for one thing, as the server's is.
- The skill and `docs/notes/server-remote.md` say where the file is and why.

## Read once, at startup

Like `config.toml`, and unlike the roster: adding a remote is a restart, and in
development the `evolve` skill. Hot-reading would put a lock in front of every
`endpoint_for`; that is a different change, and nothing here needs it yet.

## Verified

`ship/migrate/3-to-4.py` on a scratch home: two tables moved, `config.toml` keeps its
comments and loses the tables, a second run says "no remote in config.toml", and a
name that disagrees with one already in the machine file exits 1 with `config.toml`
untouched. `remote.py configure/list/--remove` against a `$GONGGA_AGENTS_HOME`.
`scripts/remote_selftest.sh` on real processes, with the file in a scratch machine root.
