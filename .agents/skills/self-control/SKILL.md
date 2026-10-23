---
name: self-control
description: 控制"自己"的运行——本回合、自己起的进程、自己的上下文、以及跑着自己的那个服务端。当你要停/续/换自己的回合、要起一个活得比自己久的作业、要跨重启或压缩保留意图、要改服务端并用上新的自己、或者发现自己的输出/进程/状态不对劲时，读这个。不涉及写业务代码。
---

# 自我控制

harness 给你四类杠杆：**回合**（turn）、**进程**（谁能收尸）、**上下文**（什么会活下来）、
**身份**（跑着你的那个 exe）。每个都有确切的作用域，其中三个是单向门。

先记住贯穿全文的一个问题：**谁回来收这活？** 有主的东西才谈得上"停"；无主的东西只有
`kill_bash` 停得住，而本回合被换掉时没有任何人回来。

---

## 1. 进程：三种 ownership

同一个 `bash` 工具，`background` / `detached` 改的不是"等不等"，是**所有权**。

| 模式 | 谁拥有 | 输出去哪 | 谁结束它 | 本回合 `cancel` 时 |
|---|---|---|---|---|
| 默认（前台） | 本回合 | 工具结果，流式 | 命令结束 / 回合取消（连进程树一起杀） | 被中断，shell 换新 |
| `background` | 那个 shell | shell 的未读缓冲，`check_bash` 取走 | `del_bash` 随 shell 一起消失 | 看取消点：卡在那条 shell 的前台命令里就一起被杀，否则活着 |
| `detached` | **没人** | 只有它自己的日志文件 | **只有 `kill_bash`** | **活着** |

实测（本会话）：

- `detached` 起了个每 1s 追加一行的循环 → `del_bash` 之后行数还在涨（13→15）；
  `list_bash` 把它单列在 `detached[]` 里，带 `pid` / `log` / `running`。
- `kill_bash --name demo-detached --job 1` → 行数 15→15 停住，`detached[]` 变空。
- `del_bash` 的回执会明说：「1 detached job(s) from them are still running and were not
  stopped」——它不会假装帮你停。

推论：

- `background` 的生死绑在 **shell** 上，不在回合上：回合结束后它还在跑；`del_bash`、
  服务端退出、或"取消时正卡在这条 shell 的前台命令里"（那条命令连同整棵进程树被杀）
  才会带走它。
- 想"后台跑、但我一收工就死" → `background`，然后别再 `del_bash` 那个 shell。
- 想"活得比我久"（守护进程、替身服务端、长跑基准）→ `detached`，然后**记下 job 号或 pid**，
  否则下一任的你会看到一个孤儿。
- 想"卡住就回来" → 前台 + `timeout_ms`（见 §3），不要自己 `&` 加轮询。

`detached` 的内部约束（读码）：独立进程组、`kill_on_drop(false)`、pid 写进 `detached_jobs` 表；
新服务端启动时 `adopt()`，但**绝不**接管 pid 等于自己或自己祖先的行——否则 `kill_bash`
就成了服务端自杀按钮。

---

## 2. 回合：三种"停"，区别全在"谁记得这活"

| 动作 | 语义 | transcript | `pending_tasks` 行 | detached 作业 | 谁接着干 |
|---|---|---|---|---|---|
| `interrupt_session --session <n>` | 别干了，丢掉 | 停在断点 | **删** | 不动 | 没人 |
| `cancel`（服务端命令） | 停，剩下的下次接着干 | 停在断点 | **留** | 不动 | 下一个启动的服务端 |
| 从外面 `taskkill` / 杀进程 | 硬死 | 停在断点 | 留 | 不动 | 下一个启动的服务端 |

### 最关键的一条：resume 不给任何提示

`resume_pending` 起的 turn 是 `turn(session, user_text = None)` —— **不写任何 instruction**，
不追加"你被中断了，继续"。它只是拿 transcript 再请求一次模型。

所以：**cancel 之前，把意图写进 transcript 或写进文件。** 否则接着干的那个你，手上只有
半截工具结果，它不知道你想干什么。可用的两种写法：

1. 发一条消息给自己（见 §5），它会在下一个 step 边界落进 transcript；
2. 写文件（`.agents/notes/`、或待办文件），并在 cancel 前用一句话把它读进 transcript。

### pending 行就是"欠下的活"

`pending_tasks` 里 `kind=turn` 表示"消息已在 transcript，只欠一个 turn"；`kind=inject`
表示"消息还没落库，谁消费谁负责写"。表里有行 = 下次启动欠着。判断自己会不会被重放：

```bash
python - <<'EOF'
import sqlite3, os
c = sqlite3.connect(f"file:{os.path.expanduser('~/.gongga/gongga.db')}?mode=ro", uri=True)
for r in c.execute("select id, session, kind, created_at from pending_tasks"): print(r)
EOF
```

### `cancel` 不会立刻杀你

`cancel` 先 `cancel_and_preserve()`，等 250ms 让回执发出去，再触发 shutdown token；监听循环
随后等 `active` 归零，**没有上限**（每 10s 打一条 warn，是观测不是砍断）。你在 select 点
（模型流、工具调用、重试退避、压缩）看到 token 才退出，所以：

- 你正卡在前台 `bash` 里 → 那条命令连同进程树被杀，你带着「已取消、退出码未知」的结果退出；
- 你正在写文件 → 那一步会跑完，然后退出。

想"立刻死、不要了"用 `interrupt_session`；想"留个干净交接"用 `cancel`。

---

## 3. 时间：`timeout_ms` 是等多久，不是干多久

超时**只结束等待，不结束命令**。实测：`timeout_ms=1000` 跑 `sleep 5`，回执是
「still running after 1s, so the wait ended -- the command was not stopped」，约 6 秒后
输出作为一条 inbox 消息自己回来了（会话里多一条 `The command you started in shell
demo-timeout ... has now finished`）。

由此：

- 超时不是预算，也不是 kill。要停就在前台等它被杀（取消），或 `kill_bash`（detached）。
- 超时后**同名 shell 换成了新 spawn 的一个，cwd 回到会话目录**。所以超时之后不要假设
  `cd` 还在；长任务的前缀请写成绝对路径或一句 `cd` 重来。
- 默认 `timeout_ms` 是 120s；`background` 忽略它。
- **没有步数上限**：一个 turn 跑到活干完。所以"我会不会失控"这件事上，harness 不给你预算，
  自我控制只能来自你：小步验证、及时落盘、别开无法收尾的长链。

---

## 4. 上下文：什么活下来，什么蒸发

| 事件 | transcript | 摘要 | 你的"当前意图" |
|---|---|---|---|
| 压缩（compaction） | 行**永不改写**，只是 `summarised_upto` 之前不再进请求 | 折成一条 summary | 可能丢 |
| 回合结束 | 保留 | — | 丢（除非写下来了） |
| `cancel` + 重启 | 保留 | 保留 | **丢**（resume 无提示，见 §2） |
| `del_session` | 全删 | 删 | 全没 |

压缩阈值默认是上下文窗口的 **70%**（本会话模型 `deepseek-flash` 窗口 1,000,000），
`set_session --compact_threshold 30%` 可调，`0` 关闭；设百分比要求模型在 roster 里有
`context`，否则命令当场报错而不是悄悄忽略。

### 记忆写在文件里，不写在上下文里

- 仓库里 `AGENTS.md` **不是自动加载的**（读码确认：服务端只发 `llm.toml` 的 `instructions`）。
  新会话要自己去看。所以重要的约定要放在**你每次都会读的地方**，或者放进 `instructions`。
- WIP 按仓库约定写 `.agents/notes/<date>-<slug>.md`。
- 长输出溢出的全文在 `~/.gongga/outputs/`，文件名是 `<session>-<shell>-<task>-<callid>`
  ——所以 `bash` 的 `task` 参数是给"未来的自己"和读 dump 的人的索引，不是装饰。
  默认超过 8000 字符就只回头尾，回执里有路径：用 `sed -n` / `grep` 读，**别 cat**。

---

## 5. 用 `send_message` 给自己留话

会话正忙时 `send_message` 不排队第二个 turn，而是写一条 `kind=inject` 的行，由当前 turn
在**下一个 step 边界**读进 transcript。这就是"给自己留一句不会丢的话"。

实测：本会话运行中执行

```bash
$GONGGA_CLI send_message --session test1 --message "SELF-NOTE: ..."
```

回执「message queued for test1; it will be read at the next step」，下一个 step 就以
一条 user 消息出现。

规矩：

- 它是**用户消息**，会一直留在 transcript 里，还会被压缩进 summary。不要当 printf 用。
- 只能用在这一处合法位置：`function_call` 和它的 `function_call_output` 之间插 user 消息
  会被 provider 整单拒绝，所以注入点在 step 边界。
- 消息自己也是 pending 行，`cancel` 前后都能活下来 —— 这是跨重启留意图最省事的办法。

---

## 6. 换掉自己（self-evolution）

`scripts/evolve.sh` 把"替换正在跑自己的服务端"排成了安全顺序：

```bash
scripts/evolve.sh stage          # 跑测试 + 构建到 .dev/target + 暂存 + 生成 launcher
# 用 bash --detached 启动它打印的 launcher
scripts/evolve.sh swap <stamp>   # 确认继任者真在等，再发 cancel
scripts/evolve.sh status         # 当前 stamp、谁在等、端口与锁
scripts/evolve.sh clean          # 收掉没人会 swap 的旧继任者
```

四条纪律（`AGENTS.md` 与脚本头部一致）：

1. **先起继任者，再 cancel**。反过来，新二进制起不来时 home 就空着，没人 resume 本回合。
2. **只有最新的 `stage` 能接管 home**。`stage` 写 `.dev/evolve-current`；被取代的 launcher
   自己退出等待循环，`swap` 拒绝非当前 stamp。重复 `stage` 安全，但别指望旧 stamp 回滚。
3. **不要用 `target/` 构建待替换的服务端**：Windows 锁住运行中的 `.exe`，而被替换的往往
   就是 `target\debug\gongga-server.exe`。`stage` 用 `.dev/target` 正是为此。
4. **`stage` 默认只构建 server**：协议没变时旧客户端照样能连；`--all` 会连客户端一起换，
   而用户正在用的 TUI 锁着 `gongga.exe`。

`swap` 的守卫（读码）：stamp 必须是 current + launcher 的 pid 还活着（`tasklist` 查的是
**Windows pid**，launcher 从 `/proc/$$/winpid` 取）+ 心跳文件 10s 内有更新 + 老服务端还活着。
**不要绕过它手动 cancel**：这些检查存在的理由就是"没人接盘时不要放掉 home"。

代价：`cancel` 之后本回合被取消并保留，继任者重放它。见 `DESIGN.md` §10 的残余风险。

日常开发换二进制用 `scripts/dev.sh`（把 exe 复制出 `target/` 再起，避免锁）。

---

## 7. 单向门：做之前先看范围

| 动作 | 不可逆的部分 |
|---|---|
| `del_session --session <n>` | 删 transcript、删 pending，**并停掉该会话的 detached 作业**（`cancel` 不这么干） |
| 从外面杀服务端 | 本回合没人接，等于丢了一次未落盘的判断 |
| 没有继任者就 `cancel` | home 空着，直到有人手动起服务端才 `resume_pending` |
| `kill_bash` | 进程没了；日志文件还在，但状态没了 |
| `del_bash` | 那个 shell 的 cwd / export / 函数全清零（后台作业一起死） |

---

## 8. 看自己

| 想知道 | 怎么看 |
|---|---|
| 我有几个 shell、谁还有没读的输出、有哪些 detached | `list_bash`（`unread_bytes`、`background_jobs`、`detached[]`） |
| 后台作业跑到哪了 | `check_bash --name <shell>`，**读即清空**，同一段不会给两次 |
| 我刚才到底做了什么、上一条命令的原文 | `get_messages --session <s> --after_seq <n> [--limit n]`（默认 50，上限 500） |
| 会话状态 / 模型 / 压缩阈值 / 消息数 | 只读查库（下面） |
| 我哪些工具调用在失败 | `tool_calls` 表按 `name,status` 汇总 |

只读查库（服务端在跑时也安全，`mode=ro`）：

```bash
python - <<'EOF'
import sqlite3, os
c = sqlite3.connect(f"file:{os.path.expanduser('~/.gongga/gongga.db')}?mode=ro", uri=True)
print(c.execute("select name,status,llm,compact_threshold,context_tokens from sessions").fetchall())
print(c.execute("select name,status,count(*) from tool_calls group by name,status").fetchall())
print(c.execute("select ID,session,kind from pending_tasks").fetchall())
EOF
```

注意服务端日志是 **stdout/stderr**，不落文件——想要日志就在起服务端时重定向。

---

## 9. 别做这些（都有代价记录）

- **一个命令一个 shell 名。** 一个名字 = 一个常驻进程；每次新名字会留下一个空转进程。
  默认就用 `default`，并在 `task` 里写清这次在干嘛。
- **把 `timeout_ms` 当 kill。** 它不杀；命令还在那个 shell 里跑，输出稍后自己回来。
- **超时后假设 cwd 没变。** 同名 shell 已经换成新的、回到会话目录。
- **`cat` 溢出文件。** 它们可能很大；用 `sed -n` / `grep`。
- **第二次 `check_bash` 期待同样输出。** 读即清空；第二次只会拿到新的部分。
- **`$GONGGA_CLI /cancel`。** `/` 只属于交互式输入行；这里 `/cancel` 会被 Git Bash 当路径
  改写成 `C:/.../Git/cancel`。命令就是裸名字：`$GONGGA_CLI cancel`。
- **`bash` 传 `cmd`。** 参数名是 `command`。多余键被忽略，缺 required 会被拒并提示
  「did you mean `command`?」。
- **在 bash 里 `kill <pid>` 停 detached。** MSYS pid 不等于 Windows pid，`taskkill` 认不出来；
  用 `kill_bash`。
- **用 `send_message` 记日志。** 它会永久留在 transcript 里并进入 summary。

---

## 10. 决策清单

动手前问四句：

1. **这活谁收？** 我死/被停之后，谁还需要它？→ 决定 `background` 还是 `detached`。
2. **我要停的是哪一种？** 丢掉（`interrupt_session`）、留着下次干（`cancel`）、还是永不回来
   （外部杀进程）？
3. **接着干的那个我，凭什么知道该干什么？** 意图进 transcript 或进文件了吗？
4. **这动作可逆吗？** 看 §7 的表；不可逆的话，范围和回退路径写在旁边。

---

## 11. 本技能的验证状态

| 结论 | 怎么来的 |
|---|---|
| 三种 bash 模式的存活语义（含 `del_bash` 后 detached 继续、`kill_bash` 停住） | 本会话实测 |
| 超时只结束等待、输出随后回投、shell 被替换 | 本会话实测 |
| 忙时 `send_message` 在下一个 step 边界投递 | 本会话实测 |
| `cancel` 保留 pending、`interrupt_session` 删 pending、`del_session` 连带停 detached | 读 `crates/server/src/commands.rs` |
| resume 不带任何 instruction | 读 `agent.rs::turn_inner`（`user_text: None` 不写行） |
| 压缩 70% / transcript 不改写 / `summarised_upto` | 读 `agent.rs`、`store.rs` |
| `evolve.sh` 四条纪律与 swap 守卫 | 读 `scripts/evolve.sh` + `status` 实跑 |
| `evolve.sh swap` 的完整换手 | **未在本会话实跑**（会换掉我）；结论来自脚本与 `DESIGN.md` §10 |
| 只读 sqlite 自查 | 本会话实测 |
