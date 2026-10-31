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
- Large too_result save to file for agents to grep instead of directly return.

### Quality of Life

- Message/notice/interruption will wake the waiting agent immediately, turn the waited process into background.

## Environment Requirement

- bash
- python, uv (package manager)
- git
- Github access

## Install

### Cargo
```sh
cargo install --git https://github.com/windwhiterain/gongga.git --locked gongga-bundle

gongga tui
```



