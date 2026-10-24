# `set_event_detail --mode all|line|one`

Status: done, 逻辑与渲染均已验（单测 + TestBackend 帧 + 真链路命令），2026-10-24.

## 三个模式

- `all`：现状，每个流一个块，文本全量追加。
- `line`：**非内容**的流块折成一行，流式时不断重写这一行。
- `one`：一个会话的所有消息合成它自己的一块；所有 cmd/cmd_result 合成一块；
  除输出消息外都是单行。

## 分层：谁决定"哪些行属于同一块"，谁决定"一块画几行"

- **命令层（cli）决定分组**：`one` 模式下同一会话的非 output part 共用一个 key
  `s:<session>`，output 用 `o:<session>:<turn>`；cmd/cmd_result 共用 `cmd`。
  其他模式仍是每个 part 一个 key。理由：分组是**语义**问题，cli 是知道语义的那层。
- **前端（tui）决定行数**：`folds(detail, kind)` 判断某块折不折；
  `Assistant`/`ToolOutput`/`ToolError` 永不折叠（那是操作者要看的内容），
  其余都是进度。`Sink::detail` 默认空实现，所以不折叠的前端可以完全不知道这回事。

## 实现要点

- `Entry` 增 `folded` / `tag` / `view_start` / `truncated`。折叠行的**标签单独存**，
  不放进 `text`：裁剪只能从前面切，标签拼进去就会被裁掉。
- `reflow_folded`：永远 1 行。整段放得下就全显示；否则取尾部 + 前置 `…`
  （预留 1 列给省略号）。
- `append_folded`：块内换行变空格；缓冲区只留最后 `FOLD_KEEP`（4096）字节——
  一行只显示尾部，前面本来也看不到，否则一小时的 thinking 会让日志无界增长。
- `one` 模式下折叠块**不因 `done` 封口**：它属于会话而不是某个 part，下一块必须复用同一行。
  标签变了就 `restart`（清空重写）——一行放不下两个事件，粘起来会读成没打过的词。
- 换模式时清空 `open` 并强制重新测量（`width = 0`），否则新块会追加到旧模式下画的块里。
- `Viewport.rows` 改成 `Cow<str>`：折叠行要拼标签/省略号，可见行数很少，分配可忽略。

## 验证

- tui：`line_folds_a_reasoning_block_onto_one_row`、`a_folded_row_keeps_its_tag_however_long_the_text`、
  `line_leaves_content_alone`、`all_keeps_every_row`、`one_rewrites_one_row_for_the_whole_stream`、
  `changing_the_mode_opens_new_blocks`、`a_folded_block_stays_one_row_at_any_width`；
  另有 `the_detail_modes_change_what_a_frame_shows`（TestBackend 真画一帧，
  三种模式在屏幕上的行数确实不同）。
- cli：分组 key（`one` vs 其他）、cmd/cmd_result 合成一块、`set_event_detail` 落盘/拒绝/提示。
- 真链路：`set_event_detail --mode line` �� `cli.toml` 出现 `event_detail = "line"`；
  `--mode huge` → exit 1 且列出可选；缺 `--mode` → exit 1。
- `cargo test --workspace` 61+59+20+143+13+5 全过；fmt / comments-check / test-layout-check 全过；
  clippy 只剩 HEAD 上既有的 3 条 server 警告。

## 一处需要你确认的判断

`one` 模式下**工具输出（bash stdout/stderr）不折叠**，仍是自己的块、全量文本。
依据是 SPEC 那句「all block is single line except output message」——
把 bash 输出折成一行会让排障看不了东西。如果你要的是「连它一起折」，改 `folds()` 一行即可。

## 顺手修的

`.worktrees/` 被另一个 session 的提交当 gitlink 入了库；本次未动，建议加进 `.gitignore`
并从索引移除（否则每个 worktree 都会在 `git status` 里出现 `m .worktrees/...`）。
