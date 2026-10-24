# 块渲染简化：窗口就是日志尾部，且空块不存在

Status: done, verified on Windows (Git Bash), 2026-10-23.

## 症状

TUI 里一堆**红色的空块**占满整屏。红色 = `LineKind::ToolError`（`LightRed`）或
`LineKind::Error`（`Red`）。

## 根因（两层）

1. **每条命令都产生一个空白 stderr chunk。** `tools/bash.rs` 的哨兵写法是
   `printf '\n<marker>%d\n'`——那个前导 `\n` 是故意的（让最后一行没有换行的命令也能干净收尾），
   但 drain 是逐行读的，于是这个空行被当成内容，作为 `text = "\n"` 的 chunk 流给前端。
   `BashStream::Stderr` → `ToolStream::Stderr` → `LineKind::ToolError` → **红色**。
   一条命令一个，几百条命令就是几百个红色空行。
2. **上一轮的"未密封块优先占位"把症状放大成占满整屏。** 那些刚打开的空块算"未密封"，
   于是拿到了窗口的预留空间，把真正的内容顶出去。

## 改法（按用户要求：只保留最简单的模型）

- **`Log::window` 回到"最后 `height` 行"**。删掉：未密封块预留、块头被裁时重打标签、
  `Cow`（行重新全是借用）。块仍然是一行，可以无限增长——增长只是把老行推到窗口上方，
  滚动回来还在。
- **空白 chunk 不许开块**：`text.trim().is_empty()` 且该 key 还没开块时直接丢弃
  （`done` 仍要清 key）。开块之后空白 chunk 照常追加，句中的空行是真实内容。
- **密封时裁掉尾部空白**：`done` 时 `trim_end()` 文本再 reflow。这样哨兵换行不会在命令
  之间留空行；句中空行保留。
- `RowIndex::add` 增加有符号的 `shift`，裁剪走增量更新，密封一个块不再重建整张索引。
- `Entry.tag` 字段删掉：标签就在块的文本里，不再需要单独存。

## 验证

- 整帧渲染探针（10 条命令 × 2 会话，含每条命令的哨兵空行，TestBackend 30×14）：
  **空白行 0 个**，输出整齐为 `[test1] ok` / `[peer] looking` / `file.txt` 循环。
- 回归测试：`a_blank_chunk_opens_no_block`、`a_sealed_block_has_no_trailing_blank_row`、
  `a_command_leaves_no_blank_block`。
- 变异验证（改完即还原）：
  - 允许空白 chunk 开块 → 上述 2 条 FAILED；
  - 密封时不裁剪 → 上述 2 条 FAILED。
- `cargo test --workspace` 全绿；`test-layout-check` 0 violations；`cargo fmt --check` 干净。
- 文档：`docs/notes/tui-log.md` 重写 `Log::window` / `Log::stream` 各节并删掉已废的
  `Entry::tag`；`docs/notes/cli-lib.md` 删掉"块头被裁会重打前缀"这条已不成立的描述。

## 取舍（明确记录）

- 单个块高过窗口时，只能看到它的尾部，**没有会话标签**（标签在块首行，已滚出窗口）。
  这是"简单模型"的代价。上一轮的重打标签是为了解决它，但用户明确要求去掉这类逻辑。
  若要兼得：让 agent 输出保持短块，或以后在状态栏/边栏标出活跃会话。
- 服务端仍会发那个空白 stderr chunk；本次在**前端**过滤掉。更彻底的做法是 drain 不把空行
  当 chunk 发（`tools/bash.rs`），但那属于 server 侧，且当时另一个会话正在改 server，故未动。
