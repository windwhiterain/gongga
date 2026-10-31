# docs/notes

The reasoning behind the comments in `crates/`, one file per source file.

`comments-check` holds a comment to 120 characters: the first line states the claim,
and only an `# Invariant` or `# Safety` section may add to it. Everything that
used to sit in a paragraph next to the code — why this shape, what was tried and
rejected, which bug it fixed — lives here instead, in the order the comments
appear in the file. A code comment that needs the argument points at the note,
as in `See docs/notes/server-store.md.`

This directory is prose, not a contract: `SPEC.md` and `DESIGN.md` are the
sources of truth. Where a note and the code disagree, the code is right and the
note is a bug.

## Index

### protocol

| note | source |
| --- | --- |
| [`protocol-lib.md`](protocol-lib.md) | `crates/protocol/src/lib.rs` |
| [`protocol-frame.md`](protocol-frame.md) | `crates/protocol/src/frame.rs` |
| [`protocol-message.md`](protocol-message.md) | `crates/protocol/src/message.rs` |
| [`protocol-channel.md`](protocol-channel.md) | `crates/protocol/src/channel.rs` |
| [`protocol-loopback.md`](protocol-loopback.md) | `crates/protocol/src/loopback.rs` |

### build

| note | source |
| --- | --- |
| [`cargo-config.md`](cargo-config.md) | `.cargo/config.toml` |
| [`bundle-main.md`](bundle-main.md) | `crates/bundle/src/main.rs` |

### server

| note | source |
| --- | --- |
| [`server-lib.md`](server-lib.md) | `crates/server/src/lib.rs` |
| [`server-main.md`](server-main.md) | `crates/server/src/main.rs` |
| [`server-agent.md`](server-agent.md) | `crates/server/src/agent.rs` |
| [`server-agents.md`](server-agents.md) | `crates/server/src/agents.rs` |
| [`server-blobs.md`](server-blobs.md) | `crates/server/src/blobs.rs` |
| [`server-commands.md`](server-commands.md) | `crates/server/src/commands.rs` |
| [`server-config.md`](server-config.md) | `crates/server/src/config.rs` |
| [`server-instance.md`](server-instance.md) | `crates/server/src/instance.rs` |
| [`server-llm.md`](server-llm.md) | `crates/server/src/llm.rs` |
| [`server-net.md`](server-net.md) | `crates/server/src/net.rs` |
| [`server-prompts.md`](server-prompts.md) | `crates/server/src/prompts.rs` |
| [`server-providers.md`](server-providers.md) | `crates/server/src/providers.rs` |
| [`server-quota.md`](server-quota.md) | `crates/server/src/quota.rs` |
| [`server-session.md`](server-session.md) | `crates/server/src/session.rs` |
| [`server-shell-command.md`](server-shell-command.md) | `crates/server/src/shell_command.rs` |
| [`server-ship.md`](server-ship.md) | `crates/server/src/ship.rs` |
| [`server-store.md`](server-store.md) | `crates/server/src/store.rs` |
| [`server-tools-mod.md`](server-tools-mod.md) | `crates/server/src/tools/mod.rs` |
| [`server-tools-bash.md`](server-tools-bash.md) | `crates/server/src/tools/bash.rs` |
| [`server-tools-detached.md`](server-tools-detached.md) | `crates/server/src/tools/detached.rs` |
| [`server-tools-index-skill.md`](server-tools-index-skill.md) | `crates/server/src/tools/index_skill.rs` |
| [`server-tools-job.md`](server-tools-job.md) | `crates/server/src/tools/job.rs` |

### cli

| note | source |
| --- | --- |
| [`cli-lib.md`](cli-lib.md) | `crates/cli/src/lib.rs` |
| [`cli-server.md`](cli-server.md) | `crates/cli/src/server.rs` |
| [`cli-bin-gongga.md`](cli-bin-gongga.md) | `crates/cli/src/bin/gongga-cli.rs` |
| [`cli-catalog.md`](cli-catalog.md) | `crates/cli/src/catalog.rs` |
| [`cli-complete.md`](cli-complete.md) | `crates/cli/src/complete.rs` |
| [`cli-commands.md`](cli-commands.md) | `crates/cli/src/commands.rs` |

### tui

| note | source |
| --- | --- |
| [`tui-main.md`](tui-main.md) | `crates/tui/src/main.rs` |
| [`tui-program.md`](tui-program.md) | `crates/tui/src/program.rs` |
| [`tui-lib.md`](tui-lib.md) | `crates/tui/src/lib.rs` |
| [`tui-input.md`](tui-input.md) | `crates/tui/src/input.rs` |
| [`tui-log.md`](tui-log.md) | `crates/tui/src/log.rs` |
| [`tui-panes.md`](tui-panes.md) | `crates/tui/src/panes.rs` |
| [`tui-render.md`](tui-render.md) | `crates/tui/src/render.rs` |
| [`tui-run.md`](tui-run.md) | `crates/tui/src/run.rs` |
| [`tui-frame_cost.md`](tui-frame_cost.md) | `crates/tui/examples/frame_cost.rs` |

### bridge

| note | source |
| --- | --- |
| [`bridge-main.md`](bridge-main.md) | `crates/bridge/src/main.rs` |
| [`bridge-chat.md`](bridge-chat.md) | `crates/bridge/src/chat.rs` |
| [`bridge-config.md`](bridge-config.md) | `crates/bridge/src/config.rs` |
| [`bridge-channel-mod.md`](bridge-channel-mod.md) | `crates/bridge/src/channel/mod.rs` |
| [`bridge-channel-qq.md`](bridge-channel-qq.md) | `crates/bridge/src/channel/qq.rs` |
