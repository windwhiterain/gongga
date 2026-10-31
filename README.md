# Gongga Harness

_Post Modern Agent Harness_

## Features

- Everything is skill, even compact context, spawn subagent.
- Heir based context compact, new session can ask mentor session about the history.
- Network traffic optimized for remote use.
- Infinite forward compatibility via chain of migrater.
- Self evolve with elegant cancle, auto continue, interrupt no session.
- Git managed configuration.
- Streaming LLM reqwest optimized for minimal allocation.

## Environment Requirement

- bash
- python
- git
- Github access

## Install

```sh
cargo install --git https://github.com/windwhiterain/gongga.git --locked \
  --features payload gongga-bundle
gongga serve --port 9700
```

That is the whole install. `cargo install` compiles the three programs into one
file, `gongga`, and its first run places them in `~/.gongga/bin` -- so a machine
that only ever receives that file needs neither Rust nor cargo.

`gongga` is the whole interface: `gongga tui` opens the window, `gongga` alone
lists what the server publishes, and a copy named `gongga-server` is the server.
`gongga --version` answers the commit it was built from.

If the one file is not what you want, `cargo install --git ... --locked
gongga-cli gongga-server` installs the programs separately; they find each other
in `~/.cargo/bin`.


