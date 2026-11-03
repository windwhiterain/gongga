# `wait` 退役：`subscribe` 是内核命令，`wait` 是 agent 的工具

日期：2026-11-03

## 症状

CLI 原来有一条客户端命令 `wait --kind K --session NAME`：读连接上的事件流，打印下一条匹配
的日志行就退出。它有两个根本问题：

- **收件人是终端，不是会话。** 想被"叫醒"的是一个**会话**（模型），而 `wait` 把行打印给了
  启动它的那个 shell。模型想等一件事只能自己轮询，或者起一个 shell 去读它的输出。
- **等待是一个进程。** 一个睡在 socket 上的进程既看不见（不在 transcript 里）、也不会随回合
  取消，还会把"谁来收这条消息"这个问题永久地留在调用方。

## 改法

SPEC 把这两件事分给了两侧，代码跟上：

- **`subscribe <名字> <pattern> [--persistant <bool>]`** 是**内核命令**
  （`crates/server/src/subscriptions.rs`）。登记完**立刻**返回；匹配时把消息投给**发起它的
  会话**，走 `send_message` 的那条路（`inject` 行），所以空闲会话会被真的叫醒，忙的会话在
  下一个 step 边界读到。
  （后来又长出两样：`--bash <名字> --cmd <cmd>` / `--python <名字> --code <code>`，匹配时在
  **会话自己的**那个同名常驻解释器里跑；以及 `del_subscribe <名字>` 按名字撤订。
  `trigger_event <名字>` 手工发一个事件，让订阅自己响。）
- **`wait {timeout_ms}`** 是**工具**（`crates/server/src/tools/mod.rs`）。agent 在回合里用它
  等到"任何东西落地"——后台任务报告、`subscribe` 的消息、人的消息——并且**什么都不消费**：
  消息仍然在下一个 step 边界按原路进 transcript，只有一条投递路径。
- **谁是 caller**：客户端从 `$GONGGA_SESSION` 读，走 `ClientInfo::session`（`#[serde(default)]`）
  到服务端。`--session` 是"命令作用于谁"，caller 是"从哪儿发出来的"，`subscribe` 是唯一需要
  把这两件事分开的命令。

## 三个坑

1. **`turn` 行不是消息。** `begin_turn` 会在 `pending_tasks` 里写一行 `kind = "turn"`，整轮都
   在那儿。第一版 `wait` 把它当成"有东西到了"，于是每次调用都立刻返回。改成只数 `inject`
   （`Store::pending_injects_of`），L3 才真的等到订阅那一条。
2. **`Event::Record` 在线上是平的**（`{"t":"record","kind":…,"payload":…}`），不是
   `{"t":"record","v":{…}}`。`wait_turn.py` 第一版按后者解析，于是永远读不到 `turn_end`，
   `"turn"` 字段一直是空的。
3. **状态先于行** 是**反过来**的：`turn_end` 行在 `finish_turn` 之前写，所以状态读到 idle 时
   行一定已经在库里——但脚本可能刚好在上一轮 poll 的 deadline 之后才去读状态，于是漏掉它。
   `wait_turn` 现在回答 idle 之前还会再问一次日志（游标之后有没有新行）。

## 内容（`ship/skills/subagent`）

`wait_turn` 原来靠 `gongga wait --kind turn_end`。CLI 没有阻塞读了，所以它改成用游标问
`events --kind turn_end --session … --since <id>`，每次最多问 `--interval` 秒，然后重读状态。
`SKILL.md` 里那段"`wait` 是个 primitive"的说明换成了新的三条路：
`subscribe`（服务端登记）、`wait`（agent 的工具）、`events`（shell 问日志）。

## 验证

`bash .dev/subscribe-check/run.sh` 与 `bash .dev/subscribe-check/wait_turn.sh`（**未入库**，
临时脚本，跑完删掉）在真服务端 + `scripts/fake_llm.py` 上给出：

- `subscribe` 立刻回答、没有 caller 时拒绝、坏正则拒绝且不落库；
- 一次 `sessions` 事件把 `subscribe \`^sessions$\` matched \`sessions\`` 送进订阅者 transcript，
  非 persistent 的订阅随即消失，persistent 的还在，`del_session` 把它带走；
- `wait` 工具在消息落地时返回（"a `subscribe` match"），并且不是等满 deadline；
- `wait_turn` 对 idle 会话立刻回答；对一个真的跑 6 秒的回合，等满之后给出
  `{"status":"idle","turn":"done"}`。

## 没做的

- SPEC 里 `--persistant` 是这么拼的（少一个 `e`）。代码照 SPEC 拼，命令目录里也是这个拼写；
  要改是一个字的 SPEC 改动 + 一处目录字符串。
- 没有 `del_subscription` / `list_subscriptions` 命令：SPEC 没写，`del_session` 与
  `--wait_ms` 已经覆盖了清理。
- `wait` 工具没有单独的上限，默认取 `[tools.bash] timeout_ms`（一个已存在的预算），
  超时不是错误、消息也不会丢。
