# 引用怎么写的知识只存一份，别处引用

User：「只 heir prompt，subagent skill 加说明，示例就行了。对了，确保 subagent 的创建也能触发
prefix cache」+「以及 self-control 里创建 agent，可以把 prefix cache 这一章内容放到一个 ship
skill 里其他地方都引用」

## 决定

- `@` 的**机制、拼写表、出错后果**只写一次：新的 ship skill `prefix-cache`
  （`ship/skills/prefix-cache/SKILL.md`）。
- 另外四处各自只留**自己那段**并引用它：
  - `context_notice`（heir prompt）：一句话 + 一个例子 + `@skill:prefix-cache`。
  - `heir` skill：第一课里的引用要点 + 指向 chapter。
  - `subagent` skill：新的一节「brief 是这个会话的第一句话」+ 一个真实 brief 例子 + 指向 chapter。
  - `self-control`（"子 agent"那一节）：一句话（brief 是第一条消息）+ 指向 chapter。
- `docs/notes/server-meta.md` 那节 `why the index is built once` 是**过期**的：index 现在每次
  请求都重建（`server-agent.md` 早就这么说）。换成实测的「哪条消息的引用能进 index」表。

## 为什么引用真的有用（不是纯省钱）

一份 skill 进了 index 之后，它正文里写的 `@skill:bar` 也进 index——`meta::roots` 是递归的。
所以 `@skill:prefix-cache` 不只是"去读那份文件"，是把正文直接带进系统提示。

`Context notice` 里写 `@skill:prefix-cache` 之所以成立，还因为 `agent::index_of` 读
`queued_texts`（队列）而不只是 `user_texts`——notice 在投递**之前**就已经被索引到了。

## 实测（一个服务端跑完）

`.dev/index-reach/probe.sh`，真服务端 + fake provider：

| 场景 | 结果 |
| --- | --- |
| 首条消息写 `@skill:chain`，chain 正文写 `@skill:leaf` | 两段正文都进 index |
| `Context notice` 里的 `@skill:prefix-cache` | 进 index（队列也算），写一行新 prompt 行 |
| 之后的消息写 `@file: late.md` | 下一次重建进 index，一行新 prompt 行 |
| 之后的消息写 `@skill: late` | 被就地换成 metadata，**永不**进 index |

`.dev/subagent-probe/probe.sh`，父会话的 bash call 里 `gongga-cli set_session` + `send_message`：

| 检查 | 结果 |
| --- | --- |
| 子 agent 的第一请求带 index（brief 的 plan.md、它目录里的 AGENTS.md、AGENTS.md 递归到的 rules.md、brief 的 skill 正文） | ok |
| 读的是**它自己的 cwd**，父会话目录的 AGENTS.md 没混进来 | ok |
| 之后两轮不写新 prompt 行，缓存 1849/1884 = 98.1% | ok |
| 目录里没有 AGENTS.md、brief 也不点名 → `index: null` | ok（这时它自己读文件，没有任何浪费） |

也就是说 self-control / subagent 描述的那条创建路径**本来就触发** prefix cache；
之前没人测过的是"notice 与后续消息各自能进多少"。

## 没做的

- 代码一行没改（包括上一轮列的 A/B/C/D/E 拼法放宽）。用户只要文档。
- `crates/server/src/tools/job.rs` 是别人刚提交的未格式化文件，`cargo fmt --check` 会报它；
  我没碰，也没提交它。
