# 客户端事件过滤器

Status: done, verified on Windows (Git Bash) in worktree `.worktrees/event-filter`, 2026-10-24.

## 症状

多会话时日志是一条交错的流：sticky `session` 解决「这条命令发给谁」，
不解决「屏幕上看谁的」。两个会话同时跑，人得在一屏里自己挑。

## 改了什么

SPEC 里 `set_event_filter --name --value` / `del_event_filter --name` 两条
client 命令落地（SPEC 拼的是 `set_sticky_cmd_argument`，和代码/DESIGN 里既有的
`persistant_cmd_argument` 对不上，按代码的拼法实现）。

- `cli/commands.rs`：`EventFilters`，与 sticky 参数同存 `~/.gongga/cli.toml`
  的 `event_filters` 段。**顺手修掉一个会咬人的前提**：`Sticky::save` 原来整文件
  重写，再加一个写入方就会把对方那段删掉，所以两个 writer 都改成
  「读盘 → 改自己那段 → 写回」。
- 匹配规则三条：每一对都要命中；事件没有该字段就放行（`session` 只约束会话流，
  不该吞掉 operator 自己敲的 `cmd`/`cmd_result`）；只滤行不滤状态
  （被滤掉的 `status` 照样更新会话列表，`sessions` 永远不滤）。
- 比较走序列化后的 JSON 字段名（`session`/`part`/`level`…），所以过滤值就是 `jq`
  里会写的那个名字，加事件变体不用加分支。
- `--name` 只收线上真实存在的字段（`EVENT_FIELDS`），其余当场拒绝并列出可选项。
- `set_session` 成功**之后**（`cmd_result ok` 才落地）把 sticky `session` 与 `session`
  过滤器一起设为该会话；`del_session` 只在删的正是过滤器指向的那个会话时才清。
  一开始写成提交时就改，被自己否了：`set_session --llm nosuch/model` 会被服务端拒绝，
  而那样客户端已经悄悄指向一个不存在的会话了。为此 `Client` 多一个 `follows`
  队列（id → 命令/会话），`handle_event` 收到 `cmd_result` 时结算。
  代价是一致性：一次性模式 `run_once` 原来自己打印 JSON、��经过 `handle_event`，
  于是 `gongga set_session` 不会再写 sticky —— 现在 `run_once` 也把事件喂给 client
  （静默），stdout 照旧只有 JSON。
- 补全：`set_event_filter --name` 列字段表，`--value` 复用 `OfNamedArgument`
  （`session` 因此补会话名）；`del_event_filter --name` 列当前过滤器，随状态重建。
- footer 增加 `| filters: session=main` 一段（没有过滤器时不显示）：一条从不出现的行
  必须在某处有解释。

## 另外修掉的一个既有缺陷

`run_once` 里 `Submission::Local` 直接 `return Ok(())`，于是客户端命令拒绝时
（例如 `del_persistant_cmd_argument --name nope`）消息进了 `NullSink` 却被丢掉，
进程仍然退出 0。现在把 `take_error()` 取出来 bail。这是在真实链路上验证过滤器参数校验
时撞出来的：`set_event_filter --name nope` 当时「成功」了。

## 验证

- 单测 55 个（原 43）：过滤器持久化与 sticky 共存、只命中、多对求交、无该字段放行、
  被滤掉的状态仍然更新、`set_session`/`del_session` 的自动行为、footer、参数校验、
  两处补全；其中一条走 `encode_server`→`handle_bytes` 的字节路径，证明过滤名就是
  线上字段名而不是只在 Rust 结构体里自洽。
- 真 UDP 往返（worktree 自己的 home，端口 9787，`:memory:` 库，按 home 本地化起停）：
  `set_event_filter --name nope` → exit 1 并列出字段表；
  `set_event_filter --name session --value main` / `--name part --value output` → 落盘且两段共存；
  `set_session --session demo` → sticky 与过滤器一起变成 `demo`；
  `set_session --session typo --llm nosuch/model` → exit 1，而 `cli.toml` 仍是 `demo`；
  `del_session --session demo` → 两条一起清掉，`part=output` 保留。
- `cargo test --workspace`（17+55+13+5+141+48）全过；fmt / comments-check /
  test-layout-check 全过；clippy 只剩 HEAD 上既有的 3 条 server 警告。
- **未验证**：TUI 真 TTY 下肉眼看到的效果（这台机器上只能人工开一次窗口）；
  一次性模式按设计不过滤，`cmd_result` 不会被吞掉导致脚本等不到回执。

## 踩到的坑（给自己留的）

`taskkill /F /IM gongga-server.exe` 会把**跑着自己的那个**服务端一起杀掉。
这次就把本会话的服务端杀了（harness 自动重启并 resume 了本回合）。杀测试服务端
只按自己记下的 pid，或者干脆用 `$GONGGA_CLI`。
