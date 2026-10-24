# 事件按 block 渲染：并行流式 = 两个同时增长的块

Status: done, verified on Windows (Git Bash), 2026-10-23.

## 问题

上一轮给流式行加了 `[session] ` 前缀，但那只在**块的第一行**。探针实测（`Log::window(10,4,0)`，
两个流并行）：

```
Reasoning nking: a1a      <- [peer] thi 被挤出窗口
Reasoning 2a3a4
ToolOutput [test1] b1
ToolOutput b2b3
```

第二条流一出现，第一条的块头就被顶掉，剩下的裸续行没有任何归属。所以"分不清谁在说话"
在**长回答流式输出时**依然存在。

## 改法：块是渲染单位

- `Entry` 就是 block：多了 `tag`（开块标签，**不**进 `text`）和 `sealed`（不再增长）。
- `Log` 自己持有 `open: HashMap<key, line>`。key 是块的**身份**，位置在开块时定死，
  所以两个块同时流式时各自原地增长，不会互换位置。
- `Tui::stream` 只剩一行委托；`stream_into` 和 `Tui.streams` 删掉了 —— 块表以前有两份
  （`Tui.streams` + `Log`），会漂移。
- `Log::window` 的分配改了，这是核心：

| 情形 | 以前 | 现在 |
|---|---|---|
| 未滚动 | 纯自底向上，按行 | 先给未密封块留出它们的行数，密封历史让位 |
| 窗口装不下所有块 | 老块的块头被裁掉、无标签 | 每个块至少 1 行，被裁的块在首个可见行重打 `tag` |
| 正在翻历史（scroll>0） | 自底向上 | 同上，但**关掉**预留（这时问的是过去） |

- 起点改用 `RowIndex::line_at`（O(log 行)）跳到窗口底行所在块，不再从日志末尾往下扫；
  否则滚动时一帧要扫整条历史，违反 `a_frame_touches_only_a_screenful` 的既有不变量。
- `Viewport.rows` 变成 `Cow`：重打标签的行是 owned，其余仍借用。

## 验证

- `cargo test --workspace` → 228 passed / 0 failed（本轮 +6）。
- 变异验证（改完即还原）：
  - 去掉流式预留 → `two_open_blocks_grow_together` / `a_squeezed_window_shows_both_blocks`
    等 5 条 FAILED；
  - `head_cut = false` → `a_cut_block_repeats_its_tag` / `a_squeezed_window_shows_both_blocks` FAILED。
- `test-layout-check` 0 violations；`comments-check` 干净（首版 4 处注释超 120 字符，已压缩）；
  `cargo fmt --check` 干净。
- 文档：`docs/notes/tui-log.md` 新增 `Entry::tag` / `Entry::sealed` / `Log::stream` /
  `Log::window — a streaming block owns its rows` 与新测试各节；`docs/notes/tui-lib.md` 把
  `stream_into` 一节改成 `Tui::stream`（那函数已删，旧文档是漂移）；
  `docs/notes/cli-lib.md` 补一条：prefix 同时是块的身份，块头被裁时会重打。

## 已知取舍

- 窗口装不下所有块时，较老的块被裁到尾部（只留可见的最后几行 + 标签）。真实终端高度下
  两个块通常都放得下；极端情况优���保证"每个块都还在屏幕上"。
- 块与块之间没有分隔线：标签 + `LineKind` 颜色已经足够区分，加分隔行会吃掉滚动历史。
