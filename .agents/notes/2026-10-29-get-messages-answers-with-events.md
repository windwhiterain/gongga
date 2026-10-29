# 读回的 transcript 是事件，不是回执里的字段（WIP）

User 的指令：「把 get_message 的返回从 cmd_result 换成重发那几个 message 的 events」，加上
随后的「不新增，复用已有事件」——后一条把第一次的实现（`Event::Message`，已合进 `3129a46`）
撤掉了。两个分叉问出来的：

- **事件形态**：词汇表还是七个，没有第八种形状要前端学。每一行按**它当初被发布成什么事件**
  重发：`text`/`reasoning`/`tool_call`/`builtin_call` → `response`（part 一一对应），
  `tool_result` → `tool_answer`（按 `call_id` 归属，正是直播那块封存的位置），
  `notice`/`system` → `log`（`agent.rs:709-719` 写 notice 那行时发的就是 `log`）。
  投递仍然**按连接**：`Outcome.events` 由 `run_commands` 写给发起方那条连接的 sink，
  不进 bus，所以别的客户端看不见你的读取。
- **`data.messages` 删掉**，bridge 改吃事件（不留双通道）。

## 落点

- protocol：新增常量 `REPLAY_TURN = 0`。直播的 `turn` 从 1 起，所以 0 让重发的行自成一个块
  ——不会并进正在被写的那条答案，也不会被 `done` 提前封口——同时是前端区分「读回来的」与
  「直播的」的唯一记号。
- server：`row_event(name, row)` 逐行决定发哪个事件（没有内容可看就不发）；
  `transcript_outcome` 的 `data` 只留 `session`/`status`/`last_seq`。快照那条不变量（行、
  状态、末尾来自同一次读）没动。
- cli：删掉 `Client::handle_message` 与 `row_kind`——读回来的行本来就落在
  `handle_response`/`handle_tool_answer` 那条路上，`log` 照旧进 cmd 窗口。
- bridge：只缓冲 `turn == REPLAY_TURN` 的 `response` part；`Pending` 多带一个 `text`
  （重发的行里用户那句话和答案同为 `output` part，只能靠「这句是我自己递交进去的」把它从
  候选答案里排除）；`since` 删除——读取改成直接问 `cursor+1:`，不再取固定尾巴窗口再本地过滤。

## 复用换来的三处降级（都写在数据里，没有藏在字段名后面）

- 行号 `seq` 不上线：逐行标签回到 `[main]`，「读到哪儿了」只看回执里的 `last_seq` 和那句
  `seq a..b`。推进游标要的本来就是它。
- `role` 没了：用户自己那句在 TUI 里是答案的颜色（直播时它由本地回显画成 User 色）。
- `notice`/`system` 落 cmd 窗口而不是会话面板，并按 cmd 块的限高只显示首行；`tool_call`
  的工具名不在 `response` 里，参数 JSON 之前没有 `bash: ` 这样的标签。

## 顺带解掉的老限制（不变）

一条回执太大就整条发不出去（`net.rs:361-374` 直接回 `ok:false`，还教用户「用 --range 少要点」）。
拆成一行一条事件之后，读一大段不再被单条消息体积打死。

## 验证

- `cargo test --workspace`：420 通过；`cargo fmt --check`、`comments-check`、
  `test-layout-check`、clippy（除 server 既有那几条）全干净。
- 真帧（`scripts/tui_live.py --script .dev/live/readback.script` → `.dev/live/readback-reuse`，
  46 帧、峰值 14 帧/秒、最大间隔 45.0ms）：`$ get_messages --range -4:-1 --session main` 之后
  `[main] hi-from-fake`、`[main] thinking: …`、`[main] The log is a window …` 落进 main 的面板，
  摘要 `4 row(s) at seq 6..9 in main (transcript ends at seq 9)` 跟在后面；`--split true` 之后
  再读一次，还是同一个面板。
- 那四行里的 system 行走 `log`，于是落进 cmd 那条限高 1 行的 strip，屏幕上只剩它的后半句
  「without a handover note, read from …」。这是 SPEC「cmd/cmd_result use a separate single
  line」的必然结果，也是这次复用换来的第三处降级里唯一肉眼可见的一处。
- 顺手修了两个从来没跑通的场景（`scripts/tui_live_scenarios.py`，随 187391c 提交）：
  `/del_event_filter` 少了 `--name session`，过滤器一直在，peer 的行全被隐藏；
  `--message peer, talk …` 少了引号，CLI 直接把整条拒掉；而 `send-message main, …` 敲下去时
  当前会话已经被上一行设成 peer。修完 `two-sessions`/`event-ui-toggles` 才真的看见两个会话交错，
  `expect [peer]` 也是第一次真的执行。

## SPEC 不用改

`events[]` 清单（SPEC.md:30-35）本来就是这七个，`get_messages` 那行（SPEC.md:50）说的
「return the rows in that closed range」在新做法下依然成立。上一版要往 SPEC 塞第八种事件的
那个问题，随撤回一起消失了。`EVENT_FIELDS` 也因此不用收 `seq`/`kind`/`role`。
