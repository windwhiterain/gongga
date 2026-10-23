---

# TUI 的流式行表明来自哪个会话

多会话同时在说话时，模型回复和工具输出没有会话名，分不清谁在说。事件本身是带
`session` 的（`ResponseEvent` / `ToolAnswerEvent`），丢在渲染侧。

## 改了什么

- `crates/cli/src/lib.rs` — `handle_response` 把 prefix 从 `"thinking: "` /
  `"tool: "` / `""` 变成 `"[{session}] "` + 原 prefix；`handle_tool_answer` 的
  prefix 从 `""` 变成 `"[{session}] "`。形状与 `Event::Status` 的
  `[{session}] idle` 一致。stream key 没动（`r:{session}:{turn}:{part}`、
  `t:{call_id}:{stream}`）。
- `crates/cli/src/lib.rs`（测试）—— 新增 `StreamSink`，它保留 prefix
  （`VecSink` 故意只留 text，没改它，免得动到既有断言）；四个测试覆盖三种
  Response part、stdout/stderr 两个 ToolAnswer 流、key 不随 tag 变化、以及
  Log/Cmd 不加 tag。
- `crates/tui/src/lib.rs`（测试）—— `a_prefix_is_written_once_per_stream`：
  同一个 key 的第二个 chunk 不再打 prefix。
- `docs/notes/cli-lib.md` — `Sink` 一节说明 prefix 每 key 只打一次，新增
  `Client::handle_response`、`Client::handle_tool_answer`、`Event::Log — no tag`
  三节，以及四个测试对应的说明节。

## 为什么把会话拼进 prefix 而不是 text

`stream_into` 在 key 已存在时只 append 新字节，prefix 只在开行时写一次。写进
文本的话每个 delta 都会重复一遍标签。

## 为什么 Log 和 Cmd 不加

服务端的 log message 通常已经带 `{name}: `（见 `agent.rs` 的
`format!("{name}: delivered a message ...")`），再加会渲染成 `[peer] peer: ...`。
`Cmd` / `CmdResult` 是当前客户端自己敲的命令和回执，不属于任何会话。

## 单会话也显示

prefix 每流只打一次，成本可以忽略；按会话数开关会让同一会话已经在屏幕上的行在
别处新建会话时突然变样，而同一个 key 的前缀必须固定。

## 验证

`cargo test --workspace` 全绿（221 通过）；`test-layout-check` 0 violations；
`comments-check --config comments-check.toml` 无违规（仓库里的配置叫
`comments-check.toml`，不是 `comments-test.toml`）。

## 复核后补的收尾测试

`crates/tui/src/lib.rs` 加 `interleaved_sessions_keep_their_own_tag`：用现成的
`Headless`（内部就是 `stream_into`）按真实 key 形状（`r:{session}:{turn}:{part}`、
`t:{call_id}:{stream}`）驱动 peer / test1 交错 8 个 chunk，断言最终 `rows()` 是
4 行、每行带自己的标签、且 peer 的 delta 落在 peer 的行上（不串到 test1）。

- 没改公开 API（`Client::new` 保持私有；cli 不能依赖 tui，layering 测试盯着），
  这条测试的边界就画在 tui 里。
- 牙口验过：把 `[test1] thinking: ` 临时改成 `thinking: ` → 测试 FAILED，diff 清楚
  指出第二行没标签；随后还原。
- fmt 对 60 列启发式会把长 `h.stream(...)` 和长元组拆成一堆碎行，所以改成
  「key/tag 各绑一个短局部变量 + 脚本表 + 循环」，既过 fmt 又是逐字可读的对话。

## 折行时标签只出现在第一行（确认，这次不改）

`Log::push` 存的是**一整行字符串**（prefix 在 offset 0），`Entry::row` 只是按
`starts` 里的字节偏移切片，`window` 按 `starts` 出可见行 —— 所以标签只在 row 0。
实测（临时探针，跑完已还原）：`[peer] thinking: ` + 60 个「一」，宽 20 时折成
7 行，**只有 1 行**含 `[peer]`，续行以正文字符开头、没有悬挂缩进。

判断：对「分不清谁在说话」这个诉求**基本够用，但有一个真实缺口**。

- 够用的部分：读历史时行首就是归属，交错的多条消息各自成行、各有标签，正是用户
  报的那个场景。
- 缺口：`window` 可以从行中间开始（`scrolling_moves_one_row_at_a_time` 就是这个
  形状），所以一条长 reasoning 流式输出时，屏幕上正在滚动的那些续行**完全没有标签**；
  如果有 2～3 个会话的长消息同时流，底部可见的续行只靠 `LineKind` 的颜色区分
  reasoning / output / tool，区分不出是哪个会话。

如果要补，三条路（按我推荐的顺序）：

1. **悬挂缩进**：续行按标签显示宽度补空格，所有折行对齐到标签之后。要动 `Log`：
   `Entry` 多存一个 `indent`（前缀显示宽度），`reflow`/`append` 的续行可用宽度要减
   掉它，现有 wrap 测试的期望行也要改。改动面中等，风险在宽度计算和宽字符。
2. **每行重复标签**：视觉上吵，且每行都吃掉标签那几列，宽字符下更容易折。
3. **左侧 session 栏（gutter）**：最彻底，但 `Log` 的 `Viewport` 行得带上 session
   值而不只是 `LineKind`，牵到 `render` 和 `log` 两层，成本大约是本轮的 3～4 倍。

本轮按指令不动。
