# `get_messages --seq`：用 slice 语法按 seq 取名段

Status: done, verified on Windows（隔离 home + 文件库 + 真 UDP 往返）, 2026-10-24.

## 需求怎么收敛的

最初提的是新命令 `get_transcript --session <s> [--last_count <n>]`。讨论中确认了三件事：

1. `get_messages` **本来就在 SPEC 和代码里**（HEAD 第 53 行，bridge 两处调用），所以
   「改名成 get_messages」是空操作，正确做法是扩展它。
2. `--limit` 的单位是 messages 表的**行**（reasoning / tool_call / tool_result / text），
   不是轮。实测 144 个 turn：每轮行数 min=1 中位=28 max=253，**62% 的轮 ≤ 50 行**。
   所以「默认 50 行 ≈ 一轮」只对平均值成立，`--limit 50` 会从轮中间切开，留下孤儿
   tool_result（实测 seq 606 的 tool_call 在窗口外）。这才是提案真正想解决的问题。
3. turn / step 都没有存进库：`turn` 只出现在事件里，`step` 是 `agent.rs:152` 的局部变量，
   只能靠「text 且 role=user」推导。**只有 seq 是持久化、每会话无空洞的**（实测
   `test1` 1..6436 / 6436 行，`tui-dev` 1..670 / 670 行）。

所以按用户定的方向落地：**单位沿用 seq，语法用 Python slice，`-n` 表示倒数**。

## 语法

`get_messages --session <s> --seq <slice>`

| 写法 | 含义 |
|---|---|
| `-1` | 最新一行 |
| `-5:` | 最新五行 |
| `5:10` | seq 5 到 10 |
| `:10` | 第一行到 10 |
| `5:` | 5 到最后 |
| `-5:-2` | 倒数第五到倒数第二 |

两处**有意偏离** Python：

- **两端都包含**。`5:10` 是 5..10。Python 的半开区间数的是元素之间的缝隙，而这里每个
  数字命名一行，排除调用方明确写出来的那个才是意外。回复永远回显实际范围
  （`3 row(s) at seq 7..9`），读者不必记约定。
- 负数是读时对 `last_seq` 解析，所以**相对下标是坏游标**：每来一轮 `-2` 指向的东西就变。
  要稳定游标用 `--after_seq`（已存在）。

拒绝：第二个冒号（`1:2:3` 是 step，这里不 step）、`0`（seq 从 1 开始）、写反的区间
（`10:5`、`-2:-5`）、只有 `:`、以及和 `--after_seq`/`--limit` 同时给
（同一个东西两种拼法）。

越界按 slice 处理：要得比有的多就是行数少（`-99:` 在 10 行库上给全部），起点超过末尾
是**成功**的空窗口并回显真实末尾。

## 实现

- `Store::messages_between(session_id, first, last)`：`seq >= ? AND seq <= ?`，两端都写死。
  `messages_after` 做不到这点——它的 limit 从尾部倒数，**没有上界**。仅当区间之外还有行时
  差异才显现，所以测试用 SQL 造了一个空洞来钉住这个契约（真实数据不会有空洞）。
- `Store::last_seq(session_id)`：负数解析要用，顺便把 `messages_after` 里那段内联查询
  的语义独立出来。
- `parse_seq_slice(raw, last_seq) -> Result<SeqWindow, String>`：纯函数，不碰数据库，
  所以语法可以逐条单测。负数是 `last_seq.saturating_add(n).saturating_add(1)`，
  `i64::MIN` 不会在 debug 下溢出。
- 上限**复用既有的 `MAX_MESSAGE_LIMIT`**（500），没有新设预算。超限**拒绝**而不是截断：
  静默截短会让 `-590:` 看起来成功了。理由仍是既有的那条——UDP 通道 1174 字节/包。

## 验证

- 单测：语法 16 个断言（含两端闭合、省略端、双负、空白）、越界钳制 5 条、非法 9 条、
  store 的 `messages_between_names_both_ends` 与 `the_upper_end_stops_the_window`、
  catalog 里 `--seq` 非必填。`cargo test --workspace` 17+55+13+5+146+51 全过。
- 真 UDP 往返（worktree 自己的 home，端口 9789，文件库，按 pid 起停）：
  `-1` / `-3:` / `5:7` / `:2` / `6:` / `-4:-2` 六种写法都返回预期 seq 区间；
  `0`、`10:5`、`1:2:3`、`:`、`x:y` 各自给出准确报错；`99:` ���空窗口；
  `-99:` 在 10 行库上给全部；`--seq` + `--limit` / `--after_seq` 被拒；
  旧写法 `--limit 2` 行为不变（bridge 依赖它）；上限边界 500 收、501 拒。
- fmt / comments-check / test-layout-check 全过；clippy 只剩 HEAD 上既有的 3 条 server 警告。

## 没做的

- 没改 SPEC（`AGENTS.md`：SPEC 未经用户允许不改）。建议那行改成：
  `get_messages --session <name> [--after_seq <n>] [--limit <n>] [--seq <a:b>]`。
- 没有绝对 turn/step 列。要做 `-2` 那种「倒数第 2 轮」的稳定寻址，得在写入时记
  `(turn, step)`：$turn 可由「text 且 role=user」回填，step 没有日志标记（只有 tool_call
  批次，缺 reasoning 时判不准），必须写入时记。
