# session 栏目重新能滚了：SPEC 的 `[session] (vertical scroll)`

用户报「左侧 session 栏目现在不能 scroll」。SPEC 那一行写的是

    - [session{name (click: ...)}] (vertical scroll)

代码里没有这个滚动：`render::list_window` 是**从 sticky session 推出来的**（把 sticky
居中），滚轮扫到列表上什么也不动（`Screen::scroll_at` 里 `if hit.part != Part::List`
直接把事件丢掉）。文档还分成两半：`tui-lib.md` 说列表「有自己的滚动（SPEC 给的）」，
`tui-render.md` 说「列表没有自己的滚动」。实现跟着后者，SPEC 跟着前者。

## 改法

列表本来就有一块 `Pane`（`Kind::List`，名字是空串），它一直带着没人用的
`scroll`/`max_scroll`。所以不新增状态，把这块盘活：

- `list_window(total, height, scroll)`：从尾部往上数 `scroll` 行，和任何窗口一个算法。
- `draw_list` 收 `&mut Screen`，用 `Pane::bound` 把上界写回（`draw_rows` 对窗口做的同一件事），
  越界就地夹住；滚开之后画返回底部的提示。
- 滚轮：`Part::List` → `Panes::scroll_list_by`，不再什么都不做。
- 点击：`bottom_at` 也答列表那一格，并且排在 `list_at` 之前——提示压着的正是列表最后一行，
  否则按下去会变成 toggle 那个会话。列表的提示报的是列表自己的 pane（空名字），所以按下是
  `pin` 列表，不会 toggle 任何会话。
- `pin_newest` 只钉读日志的 view：提交一行只往会话里追加，列表没有新行要跟，
  钉它就成了 SPEC 禁止的「scrolled from bottom 还自动滚」。

## 说明白的代价

列表不再跟着 sticky session 走：sticky 由它窗口的边框表示（就在列表右边），列表滚到哪里
是操作者自己的事。之前那条「窗口跟着名字走，所以消息的去处永远不在屏幕外」是有意的取舍，
但它和 SPEC 的 `(vertical scroll)` 只能留一个——用户要的是能滚。

## 验证

一次性的探针（`crates/tui/examples/list_scroll_probe.rs`，跑完即删；用 `TestBackend` 画真帧）：
30 个会话、80x24 → 未滚时是尾部 `s11..s30`，上界 10；滚轮 3 行后帧上是 `s08..s27`，sticky
自己的窗口没动；第 6 行点击落到画出来的那个名字上；滚开后最后一行有 `↓`，点它得到
`Part::Bottom` 并 `pin` 回底；`pin_newest` 不动列表；列表缩到 12 个时按 `s01..s12` 画，不越界。
L2：把滚轮路由改回 `if hit.part != Part::List` → 探针在 `scroll Some(0)` 变红；
把 `list_window` 改成忽略 scroll → 在「帧首应是 s08」变红。
`test-layout-check` 0 violations；`comments-check` 在我改的三处文件里只剩既有的违规
（`lib.rs` `scroll_at` 的文档、`render.rs` `draw_header` 的注，之前就有）。
