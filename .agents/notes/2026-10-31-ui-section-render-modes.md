# 按新 SPEC 的 UI 一节重做窗口：每会话一个渲染模式（WIP）

SPEC 的 UI 一节整段重写了，`Cmd` 的 client 一节也换了三个命令名。这一轮把代码对上去。

## SPEC 说了什么

- **窗口**（vertical）：
  1. horizontal：左边 sessions[]（竖排、可滚），右边 [{session{render mode = full}, events}]
     （横向分屏）；两边都「click to set sticky session」。
  2. [{session{render mode = line}, event}]：每行一个事件，竖排滚动。
  3. [{cmd, cmd_result}]：一行。
  4. stickys[{name, value}]。
  5. cmd_input：前缀选择、任意字符位置匹配补全；`/<cmd>` 或纯文本 → `send_message`。
- **scroll**：离开底部就不再自动滚动，并且要给一个「回到最底」的东西。
- **hot-key**：Shift+Enter 换行；PageUp/PageDown 滚**sticky session**；Shift+Down 滚到最底。
- **Cmd**：`set_sticky` / `del_sticky` / `set_render [--block_size]` /
  `set_render_session --session <name> [--mode none|line|full]`。

## 落点

- protocol：`client_commands()` 里换成上面四个名字；新增 `RENDER_MODES`；删掉
  `SPLIT_VALUES`、`EVENT_FIELDS`、`NO_SESSION`、`CompletionSource::SessionsOrNone`
  （它们只服务于被删掉的参数/命令）。
- cli：`RenderMode{none,line,full}` + `RenderModes`（`cli.toml` 的 `render_session`）取代
  了 `HiddenSessions` 和 `EventFilters`：**过滤按「到达」丢行，渲染模式按「绘制」丢行**，
  后者才是「设了之后先前的行也消失、再设回来整段历史回来」的那个语义。
  `Layout` 从 enum 变成 struct：`listed`（全部会话 + 状态词）、`full`、`line`、`command`。
- cli：`Sink::sticky_session(Option<&str>)`，每次 refresh 都报：窗口靠它知道 PageUp
  滚谁、点击该把 sticky 设成谁。
- tui：`Pane.kind` 分 `List/Full/Line/Command`；`areas()` 变成 SPEC 的六块
  （header / body / lines / stickys / cmd / input）；`body()` 把 body 切成列表与面板。
  只有 `Full` 面板参与 body 的均分，行组和命令行各有自己的区域。
- tui：`draw_list`（带 `▸` 标记 sticky）、`draw_lines`、`draw_stickys` 三处新绘制；
  header/help 各去掉一行（SPEC 里没有 help 行，stickys 占那一行）。
- 补全：`pick` 从 `starts_with` 改成 `contains`（SPEC「任意字符位置」），并且**前缀命中排前**
  （稳定排序），否则在完整单词上按 Tab 会把弹出框重排。
- 只显示一次回显：客户端命令自己画在命令行；服务器命令交给它自己的 `cmd` 事件（带 sticky
  填好的参数）。之前两条都画，于是每条命令在屏幕上出现两次 —— 这正是「命令行上出现没见过
  的文字」的一种。

## 两个判断，说在明处

- **没人设过的会话按 `full` 画**。SPEC 列了 `none|line|full` 但没写默认值；窗口一开什么都
  没有、又没有一行解释，读起来像服务器坏了。
- **没有 help 行**。SPEC 的窗口只有五块，hot-key 一栏改在 `docs/notes/tui-lib.md`。

## 验证

- `cargo test --workspace` 443 通过（cli 81、tui 85、protocol 26、server 230、bridge 21）。
  `test-layout-check` 0；我动过的文件 `comments-check` 与基线一致（cli/lib.rs 里 5 处是既有的）。
- 真帧 `event-ui-toggles`（120x34，63 帧）：左侧 `▸peer` 列表、右侧 peer 面板带标题、
  下面一行 `peer = full` 的命令回执、`sticky: session=peer | block_size: 2 | render: peer=full`、
  输入行照旧；每条命令只出现一次。
- **还没做**：滚动条的「回到最底」按钮目前只有 Shift+Down 这一半（`Panes::pin`），
  离开底部时还没有画出来的按钮/提示；SPEC 的 scroll 那条只完成了一半。

## 踩坑

`/set_render_session --session peer --mode line` 里的 `--session` 是**目标**，不是过滤：
我为它新写的 `drawn()` 只看会话的 mode，不看命令参数。测试里因此用 `handle_event` 而不是
直接调 `handle_response`，否则测的是绕过判断的那条路。
