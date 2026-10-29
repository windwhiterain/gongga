# `set_event_ui` 的三个参数：块高、会话过滤、分屏（WIP）

SPEC 的 UI 一节改成
`set_event_ui [--block_size <line>] [--session <name|none>] [--split <bool>]`，
并把 `set_event_filter` / `del_event_filter` 两行（含它们那句「auto set `--session`
after `set_session`」）删掉；`block_size` 的措辞也定成「max line count of **non-output**
block」——和已经实现的「回答不受限」是同一件事。

问出来两条裁定：

1. **optional 的参数一概不被 sticky 填**。`Sticky::fill` 从此只补 `required` 的。
   理由写在 `docs/notes/cli-commands.md`：可选是调用者用「没写」回答的问题。
   `send_message --session`（必填）照旧被填，SPEC 那句 auto set 仍然成立。
2. **按字段过滤的能力先留着，SPEC 暂时不加**。`set_event_filter --name --value` /
   `del_event_filter --name` 保留为通用形状；SPEC 只写会话这一种，于是
   `set_event_ui --session` 是同一个 `event_filters` 存储的会话入口，不是第二套过滤。
   并且「过滤必须是渲染的过滤，不是接收的过滤」——事件照旧收到、状态照旧更新，只是不画。

## 落点

- protocol：`client_commands()` 里 `set_event_ui` 三个可选参数，`set_event_split` 删除；
  新增 `NO_SESSION = "none"` 与 `CompletionSource::SessionsOrNone`（会话名 + 那个关掉过滤的词，
  所以既不是 `Sessions` 也不是 `Static`）。
- cli：`Client::run_event_ui` —— 三个参数先全部解析再全部落盘（`--block_size 9 --split sideways`
  整条被拒，块高不跟着改），无论读写都答一行完整设置。`set_event_split` 的分支删除。
- cli：`follow_session_command` 不再动过滤器——SPEC 那句挂在被删掉的命令上，而且 sticky 现在
  也填不进可选参数，所以 `set_session` 只管「发给谁」，不管「在看谁」。
- cli：`layout()` 让面板跟着行收窄——过滤到 main 时不再给 peer 留一块空面板（数据仍在一份日志里，
  清掉过滤就整块回来）。
- cli/catalog：`OfNamedArgument` 现在会遇到两个同名参数（`set_session --session` 是目标、
  `set_event_ui --session` 是过滤器），规则改成**优先必填的那个**，不让目录顺序决定补全。
- scripts：两个场景里的 `/del_event_filter --name session` → `/set_event_ui --session none`，
  `/set_event_split --split true` → `/set_event_ui --split true`；`event-ui-toggles` 新增一段
  「过滤到 main → `gone [peer]` → 清掉过滤 → `expect [peer]`」，让分屏 + 过滤在真帧里被走一遍。

## 副作用，说在明处

- sticky 的 `cwd` / `llm` / `thinking_effort` 这类**可选**参数不再被自动填进 `set_session`。
  今天没人这么用（`follow_session_command` 只写 `session`），但这是规则变了以后确实少掉的能力。
- 老 `cli.toml` 里若留着 `event_filters = [{name = "session", …}]`，新命令会把它读成当前过滤并
  显示在 `session = main` 位置；`set_session` 不再替你写它，所以升级后没人被悄悄过滤。
- 名字里带 `none` 的会话取不到：`--session none` 永远是「清掉过滤」。

## 验证

- `cargo test --workspace` 411 通过；`cargo fmt --check`、`comments-check`、
  `test-layout-check`、clippy（除 server 既有那几条）全干净。
- 真帧 `.dev/live/ui-toggles-merged`（87 帧，峰值 21 帧/秒，最大间隔 34.7ms）：
  `--split true` 之后 `/set_event_ui --session main` —— peer 的面板整块消失、footer 变成
  `filters: session=main`，命令行那行答 `block_size = 2  session = main  split = true`；
  `/set_event_ui --session none` 之后 peer 的面板**带着它被过滤前的全部历史**回来（一份日志
  多个视图这条没被破坏）。`two-sessions` 与 `readback.script` 也跑到底：读回来的四行仍落进
  main 的面板。
- 一次 `--scenario two-sessions` 曾在 `.dev/live-home` 没擦干净（上一次运行的服务端还占着）时
  出现「main 的面板从没出现」的假象：那是 harness 的 home 竞争，不是渲染的回归；擦掉重跑即绿。
