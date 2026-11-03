# subagent skill：起会话与订阅是同一件事，入口就是这个脚本

日期：2026-11-03 · 内容仓 `f3d3055`（`ship/skills/subagent`）· `scripts/subagent_selftest.sh`

## 用户要的

> 根据 spec 重构 subagent skill。

SPEC 的 `## Skill: subagent` 只有四条：

1. create a new session under caller session's name.
2. subscribe new session's turn end event.
3. send_message to new session.
4. optionaly wait.

## 改法

旧版把第 1 条做成了 `spawn`，第 2 条留给调用者"记得做"——而没人记得，于是父会话回去轮询。
新版让 `spawn` 一次做完前两条：

- **建会话**仍是 `set_session --must_create true`，名字仍是 `<父会话>/<角色>`、撞了加 `/2`。
- **订阅**由 `spawn` 替调用者发：`subscribe <名字> '^turn_end:<名字>$' --persistant true`，
  在返回名字**之前**完成；订阅失败则 `del_session` 撤销刚建的会话，退出 2，说清哪一步失败。
- 模式两端锚死并 `re.escape` 名字：`subscribe` 是**非锚定**的正则，`turn_end:main/impl`
  会连 `main/impl/2` 一起匹配，父会话就会为别人的回合白等。selftest 的 J 专门造一个孙会话
  来证明这条：`probe/impl/review` 结束时，`probe` 一条通知都不该收到。

**入口只剩 `spawn.py`**（`metadata.entry: spawn.py`）：

| 删掉的 | 为什么 |
| --- | --- |
| `run`（bash 分派 2 个 verb） | `gongga-cli skill <name> [args…]` 已经把 argv 原样传给入口，退出码就是入口的；两个 verb 是第二层分派 |
| `spawn`、`wait_turn`（找 python 的 bash 包装） | `skill` 的 runner 对 `.py` 本来就是 `python <path>`；包装层只是多一个会说谎的 `python3` 探测 |
| `wait_turn.py` | "等到一轮结束"是**回合里**的事：`wait` 工具就是它，超时不是错误、消息不丢、随回合取消 |

所以 `gongga-cli skill subagent impl --cwd …` 与 `"$SKILL_DIR/spawn.py" impl --cwd …` 是同一件事。

## 写正文时撞上的一件事：head 只写一次

新版 brief 那一节原来照抄旧说法（"之后的消息再写引用会在下一次重建时进 index"）。
写 selftest 时量出来是**反的**：

```
probe 的第一条 = "Start here with @file: early.md please."
  prompt row rooted = ['@file:early.md']，index 里有 EARLY 正文
probe 的后一条 = "Now also look at @file: late.md and @skill: late, then stop."
  存下的行：@file: late.md 原样；@skill: late (…\late\SKILL.md)
  同一轮请求的 instructions 里没有 LATE 正文 —— 而且全程只有 1 行 system
```

`agent::prompt_in_force` 是"有行就返回"，`index_of` 只在写那一行时跑一次
（`docs/notes/server-agent.md` §index_of 就是这么写的，理由是两个实测：改一次 head 要重付
整条 transcript）。所以：**index 只含第一条消息点名的东西**，之后写引用等于没有 index。

正文已按实测改写（brief 里一次写完），selftest 的 0/H 两条把它钉住（H 是反证：后一条消息的
引用**不该**出现在下一次请求的 instructions 里）。

**两处文档仍是旧说法，没在这次改**（不在 subagent 的范围，且都属别的 skill/笔记）：

- `docs/notes/server-meta.md` §which-message's-references-reach-the-index 的表格说"a later
  message → in the index from the next rebuild on"。
- `ship/skills/prefix-cache/SKILL.md` 的"哪条消息能进 index"表格同一行。

要改的话是一处删除 + 一句更正，`prefix-cache` 一动就要再发一次版。

## 验证

`bash scripts/subagent_selftest.sh`（真服务端 + `scripts/fake_llm.py` + 临时 home，
端口内核分配，`GONGGA_SERVER`/`GONGGA_HOME` 显式传给 spawn 的子进程）：

- 23 条断言全绿，`PASS · subagent-skill · <sha>`；
- **L2 变异**三条，都真的变红：
  - `spawn` 不订阅 → A（订阅行缺失）、B（每个子会话都该有自己那条）红；
  - 去掉 `^…$` 锚 → A 和 J（孙会话的通知漏到祖父）红；
  - 去掉 `--must_create` → B 红（第二次 spawn 又拿到 `probe/impl`，也就是共用 transcript）。
- 上一次真跑还顺带证伪了我自己的一条断言：`probe` 也会听见 `probe/impl/2`——那不是漏，
  是 `spawn` 的**调用者**是 `probe`，所以它替 `probe` 订阅了两个孩子。J 改成孙会话才测到锚。

## 一个后来的追问：`wait` 时它已经 turn end 了呢

用户指出：订阅在派活之前，所以它结束得再早，通知也已经到了——那它就不会在 `wait` 里干等。
对，这条现在写进正文了，而且量成了两个方向（`scripts/subagent_selftest.sh` 的 L/M）：

| 情形 | 结果 |
| --- | --- |
| 没有订阅（手写 `set_session` 的会话），`wait {timeout_ms: 2500}` | 坐满 **3121 ms**，回 `nothing arrived within 2.5s` |
| `spawn` 的子会话在父会话 `wait` 期间跑完 | **321 ms** 就回 `1 row(s) arrived … (a subscribe match)` |

量的时候还撞出一件事，正是那个追问的镜像：父会话**空闲**时，通知不是排队的行，而是**把这一轮
叫起来的那条 user 消息**。这一轮里再调 `wait`，队列是空的，于是必然坐满超时——回执就在同一段
上下文里，`wait` 却看不见它。所以正文多一条：**回执已经在眼前时不要 `wait`，直接读结果**。

`wait` 等的一直是"下一件"，不是"上一件"；会等空的唯一写法是**订阅挂在事件之后**
（手写订阅，或 `--persistant false` 只覆盖一轮）。

## 没做的

- 没有 `unsubscribe` / `list_subscriptions`：SPEC 没写，`del_session` 与 `--persistant false`
  已经覆盖；K 把这条边界量清楚了——删会话带走它**名下**的订阅，不带走挂在别人名下的。
- `spawn` 不再有 `wait_turn` 那种"等到结束"的封装：`events` + `wait` 两件已经够了，
  多一个脚本就是多一处要跟协议对齐的解析。
- 内容发版后服务端在下一次**启动**时安装（`evolve`），本次会话里 `~/.gongga/skills/subagent`
  仍是旧副本。
