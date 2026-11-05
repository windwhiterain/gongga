# 窗口自己保管状态，cli 一件不留

用户裁定（2026-11-05）：**agent 调用的 cli 不允许使用 client 状态，只有 TUI 允许状态**；
随后补了两句：**有 client 状态的命令不由 cli 提供，TUI 自己解析**、**schema 里也要包含候选值**。

## 之前是什么样

`cli.toml`（粘性参数、要画的会话、block size、box edge、`connect_server` 记住的地址）
由 `crates/cli` 读写，而 `gongga-cli` 这个一次性进程和 TUI 共用同一个构造入口。
于是 agent 的一次 `set_session` 会把用户的粘性 session 改掉，一次不带 `--name` 的
`send_message` 会落进用户窗口的会话里。

## 现在

- `crates/tui/src/state.rs`：`cli.toml` 的唯一读写者。
- `crates/tui/src/command.rs`：`protocol::client_commands()` 的六条由窗口解析并作答，
  行根本不到命令层；补全也分成两个目录（窗口自己的 + 服务端发布的），由 `Window` 分发。
- `crates/cli`：没有 `Sticky`/`Shown`/`BlockSize`/`BoxEdge`/`Remembered`，`build_client`
  只收 home（为了 `skills/`），`resolve_server` 不再读记住的地址，`Sink` 用
  `settled(id, ok)` 把 `cmd_result` 通报给窗口。
- 服务端也不再发布 client 命令：`Dispatch::catalog` 少了一行 `client_commands()`。

## 两个随之而来的修正

- **accepted values 进 schema**：`ArgSpec.accepts`（`Same` / `PerModel`），由服务端按 roster
  填；`CompletionSource::ModelLevels`/`ModelTiers` 删掉，`ModelInfo` 只剩名字。顺带修掉
  一个真错：roster 的 `reasoning_mapping`（`medium` → `high`）里的拼写服务端是收的，
  客户端却当成"模型不收"丢掉；现在 `accepts` 里就有它。spec 成了 roster 的快照，所以
  roster 一编辑就补发 `Event::Commands`（已实测：`medium` 现在能发出去）。
- **SPEC 的两条粘性规则**：
  - `sticky to xxx` 落成 `ArgSpec.sticky_to`，窗口在设置/清除/写回时据此忘掉跟随项
    （只认"值变了"，且只认操作者亲手打的参数——否则填充值会被当成陈述，模型一换
    `thinking` 就赖着不走）。
  - `--cache_tier none`：每个模型都收它，表示"一个 tier 都不发"；一个不声明 tier 的
    模型从此也有能打的字（`protocol::NONE_VALUE`，与 `--remote none` 一个拼法）。

## 验证（都跑过，退出码为证）

- `bash scripts/client_state_selftest.sh` → `PASS · 4s · client-state`：带粘性的 home 下
  `send_message` 无 `--name` 被拒且没进 s1、`set_session` 让 `cli.toml` 字节不变、
  五个状态命令在一次性进程里是 unknown、`--cache_tier none` 收下而 `9h` 被拒并列出 `none`。
- `python scripts/tui_live.py --scenario window-state`（真窗口、真服务端）：`/set_sticky`
  三个值各一行，换 model 后一行报 `sticky entries thinking, cache_tier cleared: they follow model`，
  第二次换 model 不再清（已经没有跟随项），`/set_render --block_size 4` 正常，
  `/del_sticky model` 之后 `cli.toml` 里 `sticky_cmd_arguments = []`。
- `cargo build --workspace`、`cargo fmt --check`、`cargo test --workspace`、
  `comments-check`、`test-layout-check`、`scripts/skill_contract.sh`、`scripts/ship.sh --check`
  全绿。

## 还没做

`scripts/cmd_selftest.sh` 里有一条（上一轮留下的、未提交的）检查断言 `skill ls` 走本地，
但 `cli::skill()` 的既定行为是"服务端发布了 `skill` 就发给服务端"，于是 `skill ls`
被当成工具调用。两者矛盾，属于上一轮的收尾，本次没有动。
