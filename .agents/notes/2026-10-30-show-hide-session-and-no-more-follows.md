# 显示哪个会话由 `show_session`/`hide_session` 说，`set_session` 不再写 sticky（WIP）

SPEC 的 client 一节这一轮改了三处，代码跟着改：

- `set_event_ui` 只留 `--block_size`、`--split`。
- 新增 `show_session --session <name>` / `hide_session --session <name>`。
- `set_sticky_cmd_argument` / `del_sticky_cmd_argument` 下面那两条「auto set/del `--session`
  after `set_session`/`del_session`」删掉，`follows` / `settle_follow` 整条路一起删。

## 为什么删 `--session` 而换成两个命令

原来的过滤是**一个名字**：`--session main` 把渲染收窄到 main，`none` 收回来。用户报的问题是
「设了 `--session` 之后，先前已经渲染出来的别的会话的事件还在」——因为过滤发生在**到达**那一侧
（`EventFilters::allows` 在 `handle_event` 里就把行丢了），而面板只是被收窄，已经进日志的行不会
消失；合并视图（`Layout::Merged`）更是没有过滤这一步。

改成 `show_session`/`hide_session` 之后语义是**按会话开合的集合**，而且必须是渲染侧的：
`HiddenSessions` 落在 cli.toml，`Client::layout` 用它算面板，`Layout::Merged` 现在带
`hidden: Vec<String>`，tui 的 `Log::set_hidden` 让合并视图不读这些窗口的块。所以
`hide_session peer` 连它已经画出来的行一起收走，`show_session peer` 又把整段历史还回来
（块一直在 `entries` 里，只是合并账本里没有它们）。

一个块因此可能只在它自己窗口的账本里、不在合并账本里（`Entry::merged_at: Option<usize>`），
`shift` 只在有位置时移动行和。

## 删掉 follow 的副作用，说在明处

`set_session` 不再替你写 sticky `session`：打字变成 prose（`send_message --message …`）时，
`--session` 是必填，没人填它了。要发消息得先
`/set_sticky_cmd_argument --name session --value main`。`scripts/tui_live_scenarios.py` 三个场景
都补了这一行，否则 `send-message` 会被拒。这是 SPEC 的决定，不是顺手改的。

## 验证

- `cargo test --workspace`：442 通过（cli 85、tui 80、protocol 26、server 230、bridge 21）。
- `test-layout-check`：0 violations。`comments-check` 在我动过的文件上干净（仓库其它文件本来就有
  若干 preamble 超限，是既有的，不是这次引入）。
- 新增测试：`crates/cli` 的 `show_and_hide_name_the_sessions_the_events_area_leaves_out`、
  `set_session_writes_no_sticky_argument`；`crates/tui` 的
  `a_hidden_session_leaves_the_merged_view_and_comes_back_whole`、
  `a_hidden_block_that_grows_does_not_move_the_rows_around_it`；`crates/tui/run` 的
  `only_a_cold_start_starts_a_server`。
- 真帧还没跑（`scripts/tui_live.py --scenario event-ui-toggles` 已按新命令改写，未执行）。
