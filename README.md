# Gongga Harness

_Post Modern Agent Harness_

## Features

- Powerful prefix cache planning argirithm. 
- Everything is skill, even compact context, spawn subagent, read_image.
  - Skills give scripts, so agents can nest, chain them arbiarily.
- Agents can do whatever user can do to the harness even where themself live.
- Auto image reference replacement, so that agent can create arbitary tool return any contents with images embedded, no need seperate `read_image` tool call.  
- Heir based context compact, new session can ask mentor session about the history.
- Infinite forward compatibility via chain of migraters.
- Elegant server cancle, restart auto continue, interrupt no session, tasks nor client, covinent for long running agents and self-evolving.
- Server watchdog auto restart server if crashed.
- Monitor session auto wakened to fix the harness when any session report an error.
- Git managed configuration.
- Streaming LLM reqwest optimized for minimal allocation.
- Large too_result save to file for agents to grep instead of directly return.
- Minimal server client traffic for cloud agent.
- Model pool with iter "flash/pro/sota/test", auto sample model based on quota remain, so agents spawn agents without need to care about quota.

### Quality of Life

- Message/notice/interruption will wake the waiting agent immediately, turn the waited process into background.
- Provider quota update in realtime. 
- Reasoning abstraction show both first line and second line so you know the reaction of the agents, and what it gonna do.

## Environment Requirement

- bash
- python, uv (package manager)
- git
- Github access

## Install

### Cargo (package manager)

```sh
cargo install --git https://github.com/windwhiterain/gongga.git gongga-bundle
gongga tui
```

### LocalBuild
```sh
cargo install --path crates/bundle --force
gongga tui
```

## Use
type this command in the tui to create your fist session "hello" with "provider/model":
```tui
/set_session --session hello --model provider/model
```