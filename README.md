# Gongga Harness

_Post Modern Agent Harness_

## Features

- Everything is skill, even compact context, spawn subagent.
- Heir based context compact, new session can ask mentor session about the history.
- Network traffic optimized for remote use.
- Infinite forward compatibility via chain of migrater.
- Self evolve with elegant cancle, auto continue, interrupt no session.
- Git managed configuration.

## Install

Two programs have to sit in one directory -- `gongga-cli` starts `gongga-server`
and only looks beside itself and in the home's `bin` -- so install them in one
command, from a tag or a commit:

```sh
cargo install --git https://github.com/windwhiterain/gongga.git --locked \
  gongga-cli gongga-server
cargo install --git https://github.com/windwhiterain/gongga.git --locked gongga-tui   # the window
```

`~/.cargo/bin` is one directory, so the two find each other. Then one start lays
out the home: `gongga-cli serve --port 9700` writes `~/.gongga/bin/gongga-cli` +
`gongga-server`, installs the pinned content and begins listening. `--rev <sha>`
pins a build the same way [`crates/server/ship.toml`](crates/server/ship.toml)
pins the prose.

A single file carrying all of it, for a machine without Rust:

```sh
scripts/bundle.sh --release      # .dev/dist/gongga: head + the programs, appended
```

## Environment Requirement

- bash
- python
- git
- Github access