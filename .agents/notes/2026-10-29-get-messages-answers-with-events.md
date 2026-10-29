# 读回的 transcript 是事件，不是回执里的字段（WIP）

User 的指令：「把 get_message 的返回从 cmd_result 换成重发那几个 message 的 events」。
两个分叉问出来的：

- **事件形态**：选「广播事件 + 按连接过滤」。实现上就是 `Event::Message`，但投递走
  `run_commands` 手里那条连接的 sink（跟 `cmd`/`cmd_result` 同一条路），所以别的客户端
  看不见你的读取；词汇表里它是普通事件，前端用画别的事件那条路画它。
  没选「复用 `response`/`tool_answer`」：那两个是**广播**（`bus.rs:13-30`、`net.rs:257-269`），
  而且没有 `seq`；用原 `turn`/`part` 重发会并进正在流式的块，并被 `done` 提前封口。
- **`data.messages` 删掉**，bridge 改吃事件（不留双通道）。

## 落点

- protocol：`Event::Message(MessageEvent{session, seq, kind, role, name, text})`。
  `seq` 是行的身份——读的人靠它推进游标，不靠「数到了几条」。
- server：`Outcome` 多一个 `events`，`run_commands` 在 `cmd_result` 之前把它们写给**发起方**；
  `transcript_outcome` 逐行造事件，`data` 只留 `session`/`status`/`last_seq`。
  快照那条不变量（行、状态、末尾来自同一次读）没动。
- cli：`Client::handle_message` → 一行一个块（key `m:<session>:<seq>`，到达即封口），
  落进那个会话的窗口；`rows_of` 删除，`row_kind` 改吃事件的字段。
  一次性模式本来就把每个事件打成一行 JSON，所以它自动就看得见行；`CmdResult` 在后面才到，
  退出时机不变。
- bridge：`Bridge::rows` 按会话缓冲 `message` 事件，结果到达时取走；**发起读取时清空**
  （probe 也会读一行，不清就会把上一次问答的行算进这一次）。

## 顺带解掉的老限制

一条回执太大就整条发不出去（`net.rs:361-374` 直接回 `ok:false`，还教用户「用 --range 少要点」）。
拆成一行一条事件之后，读 `-500:` 不再被单条消息体积打死。

## 验证

- `cargo test --workspace` 422 通过；`comments-check`、`test-layout-check`、`cargo fmt --check`、
  clippy（除 server 既有那几条）全干净。
- 真帧（`scripts/tui_live.py --script .dev/live/readback.script`）：读回的四行以
  `[main 6] hi-from-fake` / `[main 7] [prompt in force] …` / `[main 8] …` / `[main 9] …`
  出现在 main 的面板里，摘要 `4 row(s) at seq 6..9 in main` 跟在后面；分屏后再读一次同样落进
  面板。41 帧、峰值 13 帧/秒、最大间隔 40.3ms、无整屏闪烁。

## 待你定

- **SPEC 的 `events[]` 清单里没有 `message`**（SPEC.md:30-35 只列 cmd / cmd_result /
  response / tool_result）。这条改动是照你的指令做的，但 SPEC 那节现在比代码少一种事件；
  要不要我把 `message{session, seq, kind, role, name, text}` 补进 SPEC（以及把
  `get_messages` 那行改成「行以 message 事件返回，回执只报 last_seq/status」）。
  按规矩 SPEC 只有你能写，所以我没动。
- `EVENT_FIELDS`（过滤器的可选字段）还没收录 `seq`/`kind`/`role`：收进去就能
  `set_event_filter --name kind --value reasoning` 只看重读回来的思考行。要不要加。
