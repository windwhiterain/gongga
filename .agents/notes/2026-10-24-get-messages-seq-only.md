# `get_messages` 只留 `--seq`；超限报错而不是静默丢包

Status: done, verified on Windows（隔离 home + 文件库 + 真 UDP 往返）, 2026-10-24.

## 原则

「命令只管终端」。原实现里有两处违反它：

1. `--limit n`（尾部截断）、`--after_seq N`（下界）——都是 `--seq` 的等价形式，
   而 `--after_seq N --limit n` 的组合（「游标之后最新 n 行」）是一条**策略**：
   超过 n 时从下面截断，静默回答一个不同的问题。
2. 行数上限 500——拒绝**合法区间**。实测 `test1` 有 6436 行，`--seq 1:` 本来就合法。

## 改了什么

- 删掉 `--after_seq` 和 `--limit`。只剩 `--seq a:b`，且按 SPEC（你已改成不带方括号）**必填**。
  我先前给了默认 `-50:`，与 SPEC 不符，已去掉——默认区间也是一种替调用方拿主意。
  index 越界一律不拒绝：要得比有的多就是行数少。
- 删掉 500 行拒绝。唯一的上限是通道事实：单条消息 8 MiB。
- **修了一个真缺陷**：超限消息原来在 channel 的写端被 `tracing::warn!("dropping oversized
  message")` 静默丢弃，而 `sink.send()` 早已返回 Ok。调用方永远等一个不会来的回执。
  实测代价：`get_messages --seq 1:`（9.5 MB）挂 300 秒后被判超时，客户端最终
  `Error: server disconnected`。
  修法：把尺寸检查提到 `ChannelSink::send` 的 API 边界（`ChannelSink` 新增
  `max_message_len` 字段），于是超限在调用点就是 `ChannelError::MessageTooLarge`。
  server 的 `send_event` 把「带 id 的 cmd_result 太大」换成一个小回执：
  `the reply was 9527165 bytes, over the 8388608 bytes one message carries; ask for
  fewer rows with --seq, e.g. --seq -500:`。其他事件没处可说，只记日志。

## 游标搬到 bridge

删掉 `--after_seq` 后，「不重发上一轮回答」的保证不能再靠查询，必须落到调用方：

- `chat.rs` 新增 `pub fn since(rows, cursor) -> Vec<Row>`，纯函数，可单测。
- `main.rs` 读回后用 `final_reply(&since(&rows, cursor))`；探测用 `--seq -1`，
  读回用 `--seq -500:`（常量 `READ_SEQ`，不是命令的默认值）。

失效场景（测试钉住）：一轮只调了工具、没有说话时，未过滤窗口里**最后一条 assistant
text 属于上一轮**，会被当成这轮的回答重发。

已知边缘：单轮超过 500 行时，这轮较早的行读不到。对 `final_reply` 无害（答案在尾部），
只影响「只调用了工具」兜底文案能否点出工具名。

## 验证

- 单测：`rows_before_the_cursor_are_dropped`、`a_silent_turn_does_not_repeat_the_earlier_answer`、
  `a_cursor_at_the_tail_leaves_nothing_owed`、`get_messages_names_rows_one_way`
  （断言 `after_seq`/`limit` 不在目录里，且 `seq` 必填）。
  `cargo test --workspace` 20+55+13+5+144+51 全过。
- 真 UDP 往返（隔离 home，端口 9791，文件库，按 pid 起停）：
  - 700 行 / 7.92 MiB：成功（旧实现会在 500 行拒绝）。
  - 800 行 / 9.5 MiB：**2 秒内**返回上面那句 cmd_result，连接不断。
  - `-600:` 成功（600 行，超过旧上限）；`-1` = 最新一行；
    `9999:` = 空窗口并回显真实末尾；不给 `--seq` 被拒为缺必填参数。
- fmt / comments-check / test-layout-check 全过；clippy 只剩 HEAD 上既有的 3 条 server 警告。

## 留给用户的

- SPEC 第 53 行已由你改成 `get_messages --session <name> --seq <a:b>`，与实现一致
  （不带方括号 = 必填，实现也改成必填）。
- 没做的事：`--seq` 没有绝对 turn/step 列可依，`-2` 这类相对下标仍是坏游标
  （稳定游标靠 `--seq (n+1):`）。
