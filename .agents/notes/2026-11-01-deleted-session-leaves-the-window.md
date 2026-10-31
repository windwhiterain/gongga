# 删掉会话时，客户端那边的行也一起删

用户报的是：`del_session` 之后客户端（tui）里那个会话的行还在。服务端是干净的
——`Store::delete_session` 早就 `DELETE FROM events WHERE session_id`——所以漏的是**显示那一侧**。

## 为什么不能靠 layout

`Sink::layout` 每次 `refresh` 都发，但它说的是**画哪些窗口**，不是**还有哪些会话**：
`hide_session` 让一个会话没有面板、合并视图里也没有它的块，可它的行必须留在日志里，
`show_session` 要原样还回来。用 layout 做「删了」的判断等于把隐藏也当成删除。

真正的信号只有一个：两个 `Sessions` 公告（greeting 和流式 `Event::Sessions`）都带**全部**
会��，所以旧列表里有、新列表里没有的名字就是被删的那个。`Client::set_sessions` 在替换列表
前比较两者，对每个消失的名字调一次新的 `Sink::forget(window)`。

## 为什么要清，而不只是不管它

服务端会把名字再发出去（`sessions.name` 没有唯一历史），同名的下一个会话必须从**它自己的
第一行**开始。所以 `Log::forget` 把那个窗口的块、它的账本、以及还在它里面开着的流（`open`
的 key）一起丢掉，再 `rebuild`；`Panes::forget` 连面板一起删，免得新会话继承旧视图的 scroll。

`Log::forget` 是这里唯一会拿走行的地方——cap 只是让某些行不进视图，`set_hidden` 一行都不丢。

## 第二个洞：`del_session` 的回答没有窗口了

`finish_delete` 先 `publish_sessions` 再回 `CmdResult`，所以回答到达时它被路由去的那个窗口
已经属于一个不存在的会话：写进去看不见（没有面板画这个名字了），还会在那个名字下面留一行，
正是下一个同名会话会读到的第一行。`Client::answer_window` 因此只在窗口仍是「客户端画的东西」
（活着的会话，或命令行）时保留它，否则落到命令行上。

## 验证

- `cargo test --workspace` 通过（cli 94、tui 112、server 248，其余不变）。
- `test-layout-check`：0 violations；`comments-check` 在改过的文件上没有新增违规
  （`crates/cli/src/lib.rs` 仍是既有的那 9 条 preamble 超限）。
- 新增测试：`a_deleted_session_forgets_its_window`（流式与 greeting 两条路）、
  `an_answer_to_a_deleted_session_goes_to_the_command_line`、
  `forgetting_a_window_leaves_nothing_of_it`（log）、`forgetting_a_window_empties_its_pane`（tui）。
