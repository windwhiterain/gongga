# agent 直接和 server 说话，不再绕 CLI

## 病灶

agent 就住在 server 进程里，可是它驱动 harness 的唯一方式是 **spawn 一个 CLI 子进程，
让这个子进程通过 UDP 连回自己所在的 server**：

```
提示词： "you can drive it from bash with the gongga CLI: `$GONGGA_CLI <command>`"
```

一圈网络、一个进程、一次握手，去调一个同进程里的函数。而且它把 agent 和
"机器上装了 gongga 二进制、且这个二进制连得上" 绑在一起 —— `cli_path()` 找不到时
只打一条 warn，agent 就没有驾驶盘了。

## 改法

1. 新工具 `harness{command?, arguments?}`（`crates/server/src/tools/harness.rs`）：
   直接 `crate::commands::dispatch()`，同进程、无子进程、无 UDP。
   - 不带 `command` 就列目录 —— 列表来自 `Dispatch::catalog()`，即每个客户端 greeting
     里那份，**不在这里再存一份**（否则又回到上一轮删掉的双清单）。
   - `arguments` 是 `{名字: 值}`（不带 `--`），值统一转字符串；`null` 表示"没给"。
   - 命令名前面的 `/` 会被 strip：模型在文档里读过那种写法，拒绝它没有教育意义。
   - 回包超长时按 bash 同一套规则截断 + 落盘 —— 进程内没有 MTU 那道上限，否则
     `get_messages --range 1:` 会整段进 transcript，之后每一步都再付一次钱。
   - client 命令（`set_sticky_cmd_argument` 等）由 `dispatch` 自己拒掉：它改的是调用者
     自己的状态，agent 不是那个调用者。这条是命令面的性质，不是工具里的检查。
2. **循环引用**：`Dispatch` 拥有 `Agent`，`Agent` 拥有 `ToolRegistry`，registry 拥有这个工具。
   所以工具持 `Weak<Dispatch>`，`main` 用 `Arc::new_cyclic` 造 dispatch。
3. **提示词**改教工具；`$GONGGA_CLI` 留着，但只作为**脚本**的通道（server_script、
   evolve launcher、skill 脚本）—— 它们是独立进程，本来就没有 agent 的工具。
4. `.agents/skills/self-control/SKILL.md` §5 / §9 同步：自己发消息用工具，脚本用 CLI。

## 一个必须说清的边界

工具**不能**打断 agent 自己正卡住的那条 `bash`：一个 turn 一次���跑一个工具，卡在
`bash{sleep 40}` 里就调不了 `harness`。所以"叫醒自己那条 sleep"只能用**别的进程**
（另一个会话、或一个脚本）发消息。旧 skill 里那句"你自己排队发的消息也会打断自己正在等的东西"
只在这个意义下成立，已经写清楚。

## 证据

- `cargo test --workspace`：20+70+15+5+169+63 全过（新增 8 个 harness 工具测试）。
- 关键测试 `a_command_needing_nothing_answers_in_process`：一个只差监听的真 dispatch，
  **没有 socket、没有 CLI**，`cancel` 能答 —— 说明它真的走的是 `dispatch`。
- `a_command_runs_in_process_and_its_data_comes_back`：`set_session` 写行、
  `get_messages` 读回 data（`last_seq`/`status`/session），然后直接查 store 确认。
- `the_instructions_send_the_agent_through_the_tool`：提示词里有 `harness`，
  没有 "drive it from bash with the gongga CLI"。
- 真链路（fake LLM 让模型自己发起工具调用）：
  - `harness{set_session via-tool}` → transcript 里 `tool_call|harness` + `tool_result`，
    直接读 SQLite 确认 `via-tool` 真的建了；
  - `harness{cancel}` → **806µs**（CLI 那条路约 250ms），turn `Cancelled`、
    server 正常退出、shells 关闭、pending 行留给下次启动。
- `comments-check` 0、`test-layout-check` 0、`fmt` 干净；clippy 剩下的 5 条都在
  `llm.rs`/`session.rs`/`agent.rs`（本来就有）。

## 没做的

CLI 本身一个字没删：脚本、bridge、人都在用它。改的只是**agent**该走哪扇门。
