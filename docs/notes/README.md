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

## By task

The index below is by source file: read a note when you edit the file it belongs
to. This one is by task, because the note that holds a hazard is not the file
you are about to touch.

| you are about to | read first |
| --- | --- |
| run the distribution (`gongga`, `gongga tui`) | [`bundle-main.md`](bundle-main.md) -- it is not a read-only run: pointed at a live home it stops that home's server |
| start, stop or watch a server | [`link-server.md`](link-server.md), [`server-supervise.md`](server-supervise.md), [`server-signal.md`](server-signal.md) |
| write into a home (payload, config, database) | [`server-config.md`](server-config.md), [`server-store.md`](server-store.md) |
| change when the server says the disk is low, or what wakes `@monitor/disk` | [`server-disk.md`](server-disk.md), [`server-monitor.md`](server-monitor.md) |
| change what a client sends or receives | [`link-lib.md`](link-lib.md), [`protocol-lib.md`](protocol-lib.md) |
| change the window | [`tui-lib.md`](tui-lib.md), [`tui-run.md`](tui-run.md) |
| change what the window remembers (`cli.toml`) | [`tui-state.md`](tui-state.md), [`tui-command.md`](tui-command.md) |

## Index

### protocol

| note | source |
| --- | --- |
| [`protocol-lib.md`](protocol-lib.md) | `crates/protocol/src/lib.rs` |
| [`protocol-frame.md`](protocol-frame.md) | `crates/protocol/src/frame.rs` |
| [`protocol-message.md`](protocol-message.md) | `crates/protocol/src/message.rs` |
| [`protocol-reference.md`](protocol-reference.md) | `crates/protocol/src/reference.rs` |
| [`protocol-channel.md`](protocol-channel.md) | `crates/protocol/src/channel.rs` |

### skill

| note | source |
| --- | --- |
| [`skill-lib.md`](skill-lib.md) | `crates/skill/src/lib.rs` |

### python

| note | source |
| --- | --- |
| [`python-client.md`](python-client.md) | `python/gongga.py` |
| [`skill-entry.md`](skill-entry.md) | `python/gongga_skill.py`, `crates/skill/src/entry.rs` |

### console

| note | source |
| --- | --- |
| [`console-lib.md`](console-lib.md) | `crates/console/src/lib.rs` |

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
| [`server-clock.md`](server-clock.md) | `crates/server/src/clock.rs` |
| [`server-commands.md`](server-commands.md) | `crates/server/src/commands.rs` |
| [`server-config.md`](server-config.md) | `crates/server/src/config.rs` |
| [`server-diff.md`](server-diff.md) | `crates/server/src/diff.rs` |
| [`server-disk.md`](server-disk.md) | `crates/server/src/disk.rs` |
| [`server-instance.md`](server-instance.md) | `crates/server/src/instance.rs` |
| [`server-llm.md`](server-llm.md) | `crates/server/src/llm.rs` |
| [`server-meta.md`](server-meta.md) | `crates/server/src/meta.rs` |
| [`server-net.md`](server-net.md) | `crates/server/src/net.rs` |
| [`server-pictures.md`](server-pictures.md) | `crates/server/src/image/pictures.rs` |
| [`server-prefix-cache.md`](server-prefix-cache.md) | `crates/server/src/prefix_cache.rs` |
| [`server-process.md`](server-process.md) | `crates/server/src/process.rs` |
| [`server-prompts.md`](server-prompts.md) | `crates/server/src/prompts.rs` |
| [`server-providers.md`](server-providers.md) | `crates/server/src/providers.rs` |
| [`server-quota.md`](server-quota.md) | `crates/server/src/quota.rs` |
| [`server-monitor.md`](server-monitor.md) | `crates/server/src/monitor.rs` |
| [`server-session.md`](server-session.md) | `crates/server/src/session.rs` |
| [`server-shell-command.md`](server-shell-command.md) | `crates/server/src/shell_command.rs` |
| [`server-ship.md`](server-ship.md) | `crates/server/src/ship.rs` |
| [`server-store.md`](server-store.md) | `crates/server/src/store.rs` |
| [`server-subscriptions.md`](server-subscriptions.md) | `crates/server/src/subscriptions.rs` |
| [`server-supervise.md`](server-supervise.md) | `crates/server/src/supervise.rs` |
| [`server-tools-mod.md`](server-tools-mod.md) | `crates/server/src/tools/mod.rs` |
| [`server-base64.md`](server-base64.md) | `crates/server/src/base64.rs` |
| [`server-remote.md`](server-remote.md) | `crates/server/src/remote/` |
| [`server-tools-bash.md`](server-tools-bash.md) | `crates/server/src/tools/bash.rs` |
| [`server-tools-detached.md`](server-tools-detached.md) | `crates/server/src/tools/detached.rs` |
| [`server-tools-tunnel.md`](server-tools-tunnel.md) | `crates/server/src/tools/tunnel.rs` |
| [`server-tools-text.md`](server-tools-text.md) | `crates/server/src/tools/text.rs` |
| [`server-tools-skill.md`](server-tools-skill.md) | `crates/server/src/tools/skill.rs` |
| [`server-pylib.md`](server-pylib.md) | `crates/server/src/pylib.rs` |
| [`server-tools-words.md`](server-tools-words.md) | `crates/server/src/tools/words.rs` |
| [`server-tools-job.md`](server-tools-job.md) | `crates/server/src/tools/job.rs` |
| [`server-transcript.md`](server-transcript.md) | `crates/server/src/transcript.rs` |

### link

The client side `crates/cli` and `crates/tui` share: one wire client, one command
surface, two front-ends.

| note | source |
| --- | --- |
| [`link-lib.md`](link-lib.md) | `crates/link/src/lib.rs` |
| [`link-server.md`](link-server.md) | `crates/link/src/server.rs` |
| [`link-catalog.md`](link-catalog.md) | `crates/link/src/catalog.rs` |
| [`link-complete.md`](link-complete.md) | `crates/link/src/complete.rs` |
| [`link-commands.md`](link-commands.md) | `crates/link/src/commands.rs` |
| [`link-skill-command.md`](link-skill-command.md) | `crates/link/src/skill_command.rs` |

### cli

| note | source |
| --- | --- |
| [`cli-bin-gongga.md`](cli-bin-gongga.md) | `crates/cli/src/bin/gongga-cli.rs` |

### tui

| note | source |
| --- | --- |
| [`tui-cache.md`](tui-cache.md) | `crates/tui/src/cache.rs` |
| [`tui-main.md`](tui-main.md) | `crates/tui/src/main.rs` |
| [`tui-program.md`](tui-program.md) | `crates/tui/src/program.rs` |
| [`tui-lib.md`](tui-lib.md) | `crates/tui/src/lib.rs` |
| [`tui-fold.md`](tui-fold.md) | `crates/tui/src/fold.rs` |
| [`tui-input.md`](tui-input.md) | `crates/tui/src/input.rs` |
| [`tui-log.md`](tui-log.md) | `crates/tui/src/log.rs` |
| [`tui-markdown.md`](tui-markdown.md) | `crates/tui/src/markdown.rs` |
| [`tui-notice.md`](tui-notice.md) | `crates/tui/src/notice.rs` |
| [`tui-panes.md`](tui-panes.md) | `crates/tui/src/panes.rs` |
| [`tui-render.md`](tui-render.md) | `crates/tui/src/render.rs` |
| [`tui-table.md`](tui-table.md) | `crates/tui/src/table.rs` |
| [`tui-tree.md`](tui-tree.md) | `crates/tui/src/tree.rs` |
| [`tui-run.md`](tui-run.md) | `crates/tui/src/run.rs` |
| [`tui-state.md`](tui-state.md) | `crates/tui/src/state.rs` |
| [`tui-command.md`](tui-command.md) | `crates/tui/src/command.rs` |
| [`tui-frame_cost.md`](tui-frame_cost.md) | `crates/tui/examples/frame_cost.rs` |

### bridge

| note | source |
| --- | --- |
| [`bridge-main.md`](bridge-main.md) | `crates/bridge/src/main.rs` |
| [`bridge-chat.md`](bridge-chat.md) | `crates/bridge/src/chat.rs` |
| [`bridge-config.md`](bridge-config.md) | `crates/bridge/src/config.rs` |
| [`bridge-channel-mod.md`](bridge-channel-mod.md) | `crates/bridge/src/channel/mod.rs` |
| [`bridge-channel-qq.md`](bridge-channel-qq.md) | `crates/bridge/src/channel/qq.rs` |

### reference

`docs/reference/` holds what the *providers* define rather than what we chose: the shapes
this server must fit, with sources and fetch dates.

| doc | what it is |
| --- | --- |
| [`../reference/openai-responses.md`](../reference/openai-responses.md) | the Responses API this server speaks |
| [`../reference/prefix-cache.md`](../reference/prefix-cache.md) | cache modes, lifetimes and billing, per vendor |
