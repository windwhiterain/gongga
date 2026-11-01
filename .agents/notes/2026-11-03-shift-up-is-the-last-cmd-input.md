# `Shift+↑` 才是「上一条命令」，裸 `↑↓` 只管输入框里走行

日期：2026-11-03

## 症状

单行输入时按 `↑`，上一个已经发出去的命令行被塞回输入框。多行输入里 `↑` 又是「上一行」：
同一个键，按光标在哪儿干两件不相干的事。而单行那一件恰好是手最容易按到、也最不该误触的——
输入框里正打着字，一按 `↑` 就把草稿换成了上一条命令。

## 改法

SPEC 的 hot-key 已经写了 `Shift+Up: last cmd input`，代码跟上：

- `Tui::arrow_key`：只认键位，把 SPEC 在 `↑↓` 上的三条绑定分开——`Shift+↑` 是上一条命令行、
  `Shift+↓` 回底部、裸 `↑↓` 是输入框的上一行/下一行。
- `Tui::arrow`：执行。补全弹窗仍然先吃箭头（人正看着它），然后才轮到文本。
- `history_next` 删掉：没有键再往前走。往回走停在最老的一条，`Shift+↓` 是 SPEC 的「回底部」，
  不再兼作「下一条历史」。
- 帮助行补一条 `Shift+↑ last cmd`：SPEC 的六个 hot-key 都在提醒里，顺序照 SPEC。

`Shift+↑` 原来那点「往上滚一行」的老绑定随之消失（`Alt+↑` 翻页、滚轮带 Shift 才是滚一行）。

## 验证

- 新增 `the_vertical_arrows_are_specs_bindings`：三条绑定各自的键位，以及 `←→ / Enter / 字母`
  都不是。L2 撤掉修复（裸 `↑` 改回 `LastCmd`）能变红。
- `cargo test --workspace` 全绿；`test-layout-check`、`comments-check` 无新增违规；
  `cargo fmt --check` 干净。

## 帮助行变长，几处按帧算的断言跟着挪

列表从 117 列变成 136 列：宽度 40 的窗口提醒是 5 行（原 4），宽度 120 的窗口是 2 行（原 1）。
所以 `the_header_says_whose_server_it_is`、`the_block_size_changes_what_a_frame_shows`、
`a_click_on_the_way_back_to_the_bottom_pins_the_view` 各加一行窗口高；两个 120 宽的滚动测试
改到 140 宽（那里本来就写着「宽到提醒只占一行」）；`the_reminder_wraps_to_the_rows_the_window_allows`
的 117/116 改成 136/135。它们测的不是提醒宽度，是滚动与面板——把窗口尺寸调回去，断言就不用改。

## 没做的

- 没给「下一条历史」留键：SPEC 只给了一个方向，硬塞一个（比如又被 `Shift+↓` 占着）就是自己编键位。
- 没动真终端手感：`Shift+↑`/`Shift+↓` 依赖终端报修饰键，这一层只能人工开一次窗口看。
