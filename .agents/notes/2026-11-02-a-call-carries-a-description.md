# `bash` 的 `task` 改成 `description`，工具调用多一个 `description` 标签

SPEC 这一轮把两件事一起改了：bash 的 `task` 参数改名 `description`，并且把「一个工具调用
最多带一个 `description`」写进了 Tool 一节，`abstract` 那一行从
`toolcall{tool_name, {name, task, …}}` 变成 `toolcall{tool_name·description, {name, background, …}}`。

## 为什么不是把 `task` 塞进字段列表

折叠行只有一行。`name=default, background=false, detached=false` 这类默认值对「这次在干嘛」
没有信息量，而模型自己写的那句话才是这一行的主语。所以它不是一个 `Field`：
`Field { label, value, summary }` 要求每个字段有个名字，`description` 的名字由这一行本身给了
（`bash·check the state`），再当字段就等于同一句话说两遍，而且是带 label 的那一遍。

`Head::title()` 是唯一拼 `·` 的地方，`summary()`（TUI 折叠行、cli 的 `head_label`）和
`detail()`（展开、一次性 cli 输出）都从它出发。这样两个 row 不会对「名字到哪结束」有分歧。

## 服务端

- `agent::call_head` 解析一次 arguments，`description_of` 从里面取 `description`，
  `call_fields` 只收 `serde_json::Value` 不再自己再解析一遍。
- `call_fields` 的 `owns_row("command" | "description")`：这两个已经各自有行了（详情第一行、
  标题），不再当普通字段列一遍。
- `summary_keys("bash")` 去掉 `task`，留 `{name, background, detached, timeout}`，
  也就是 SPEC 现在的 `abstract` 里那四项。
- 溢出文件名跟着改：`<session>-<shell>-<description>-<callid>`；`spill_label` 的参数同步改名。
- replay 走同一个 `call_head`，所以旧 transcript 的行只要 arguments 里带 `description`
  就同样显示 —— 不需要迁移。

## 未做：ship 里的 skills/self-control 仍然写 `bash` 的 `task`

`ship/skills/self-control/SKILL.md` 第 168–169、265 行还在说 `task` 参数（溢出文件命名、
「在 `task` 里写清这次在干嘛」）。改它要在 `ship/` 子模块里提交并重新 pin（`scripts/ship.sh`，
要推远端），所以等用户点头再做。

## 验证

- `cargo check --workspace --all-targets`、`cargo fmt --check`、`comments-check`（改过的三个
  文件与 HEAD 基线比，没有新增 violation）、`test-layout-check`：都干净。
- 真模型跑了一遍 TUI（`scripts/tui_live.py`，用本机 roster 的 deepseek），折叠行是
  `▸ bash·run one bash command and report its output` —— 模型自己写的 description。
- 同一份 home 用 `gongga-cli get_messages` 读回 replay 的 `tool_call` 事件：
  `{"kind":"call","name":"bash","description":"run one bash command and report its output",
  "fields":[{"label":"","value":"echo …","summary":false}]}` —— description 只出现一次，
  没有变成一个字段。
