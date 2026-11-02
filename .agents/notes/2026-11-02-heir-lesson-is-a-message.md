# 第一课是一条消息，不是一列

User：「handover 居然是一个 rust 里的概念吗，不行啊，handover 必须用一条 send_message 模拟」。

## 之前是什么

`sessions.handover` 一列 + `handover_preamble` 模板 + `llm::build_head` 的一个 input item。
三处后果其实是一条：**服务端知道一种 SPEC 里没有的东西**。

- 它的 `@` 引用既没进 index（`index_of` 只读 `kind='text'` 的 user 行），也没被展开
  （`expand` 只处理 skill/image）。第一课里写 `@file: plan.md`，等于把一个本该只收一次费的
  文件，变成继承人第一轮自己去读。
- 每加一个读者就要教一次：哪个模板、什么生命周期、哪个占位符。

## 现在是什么

`heir` 把会话删空之后跑一次 `send_message --session <自己> --message <第一课>`
（`$GONGGA_CLI`，服务端已导出到每个 shell）。因为这条命令跑在它必须保留的那个 call 里，
消息走 `inject` 队列，在下一个 step 边界由 `Store::deliver_pending` 落成一条普通 `text` 行。

服务端那一半全部删除：`Prompt.handover`/`stamp`、`Store::handover`、`build_head` 的 note item、
`prompts::REQUIRED` 的 `handover_preamble`、`ship/config/config.toml` 的模板、`system` 行的
display 文案。`sessions.handover` 列也删除，旧 home 的笔记由 `store::settle_handover` 在一个
事务里回填成该会话的一条 user 行（追加在末尾，绝不插到已发的行前面），然后 `DROP COLUMN`。

顺带解决的是上一轮探针发现的两个洞：第一课现在**在** index 里（`meta::roots` 从
`user_texts` 里读到它），而且清空后的首条消息与全新会话首条消息走同一条路（没有 prompt 行
可依，`literal` 从消息自身算 roots，`@skill:` 留字面而不是被内联）。

## 为什么不是脚本直接插一行

`heir` 跑在"必须保留的那个 call"里面。此刻插一条 user 行，它就落在 call 和它的 result 之间，
那是 provider 不收的形状。`send_message` 在 turn 忙时正是写 `pending_tasks` 的 `inject` 行，
由服务端自己决定什么时候可以落——这不是第二条规则，就是 `deliver_pending` 那条。

## 发不出去时

会话此刻已经空了，所以第一课是唯一的一份：`send_lesson` 失败就把第一课原样写进 stderr
（也就是这条 call 的 tool_result）并非零退出。静默丢课是这个命令唯一不可恢复的失败。

## 实测

`bash .dev/heir-live/probe.sh`（一个 fake endpoint、一个真服务端、真 `gongga-cli`、
真 `transcript.py` 跑在 bash call 里）：

| 检查 | 结果 |
| --- | --- |
| 旧 home 的笔记变成它的第 1 条消息，列消失 | ok |
| 第一课落成普通 user 行（工具调用/结果之后） | ok，seq 6 |
| 第一课的 `@file: plan.md`、`@skill: probe` 成为 index root | ok |
| 后续一轮不再重写 prompt 行 | ok，仍是 1 行 |
| 后续一轮的缓存命中 | 2016/2056 = 98.1% |
| mentor 里是完整 archive，`events` 记了一条 handover | ok |

`bash scripts/heir_selftest.sh`：7 个用例全绿（含新增的"发不出去就写进结果"）。

## 顺手修掉的过期断言（都不是这次改的）

- `test_heir.py` 断言 `config.toml` 里有 "Issue independent read-only tool calls…" 与
  "cascade them with `&&`"：这句话 09537ef 就搬去 `whiting-dev` skill 了，测试从那天起是红的。
- `scripts/replay_audit.py` 的 prose 锚点 "Do not ask for confirmation; the user has already
  authorised bash execution." 也早就不在 config 里了；改为现存的 "do not ask for confirmation,
  and leave nothing behind."。
- `ship/skills/heir/SKILL.md` 说"`heir` 之后 seq 从 1 重新开始"——脚本只删行、不重编号，
  在飞的 call 保留原 seq（实测 seq 3 的 call 留下来）。已改成"没有在飞的 call 时从 1 开始"。

## 没做的

- SPEC 没动。它写的 "for the first user message to a empty session" 现在正好对上：清空后的
  第一条就是这个第一课。
- 本机 `~/.gongga` 里没有任何会话的 `handover` 非空，所以这次没有"真实的旧 home"可迁移；
  迁移只在探针里用旧 schema 造过。
- 工作树里 `SPEC.md` 有一处不是我的改动（Meta 那一节），提交时没有带上。
