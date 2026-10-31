# Gongga Harness

_Post Modern Agent Harness_

## Features

- Everything is skill, even compact context, spawn subagent, read_image.
  - Skills give scripts, so agents can nest, chain them arbiarily.
- Auto image reference replacement, so that agent can create arbitary tool return any contents with images embedded, no need seperate `read_image` tool call.  
- Heir based context compact, new session can ask mentor session about the history.
- Network traffic optimized for remote use.
- Infinite forward compatibility via chain of migraters.
- Elegant cancle, restart auto continue, interrupt no session nor tasks, covinent for long running agents and self-evolve.
- Git managed configuration.
- Streaming LLM reqwest optimized for minimal allocation.
- Large too_result save to file for agents to grep instead of directly return.

### Quality of Life

- Message/notice/interruption will wake the waiting agent immediately, turn the waited process into background.
- provider quota update in realtime. 

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



