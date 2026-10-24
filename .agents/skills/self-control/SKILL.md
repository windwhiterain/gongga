---
name: self-control
description: 控制"自己"的运行——本回合、自己起的进程、自己的上下文、以及跑着自己的那个服务端。当你要停/续/换自己的回合、要起一个活得比自己久的作业、要跨重启或压缩保留意图、要改服务端并用上新的自己、或者发现自己的输出/进程/状态不对劲时，读这个。不涉及写业务代码。
---

# 自我控制

harness 给你四类杠杆：**回合**（turn）、**进程**（谁能收尸）、**上下文**（什么会活下来）、
**身份**（跑着你的那个 exe）。每个都有确切的作用域，其中三个是单向门。

先记住贯穿全文的一个问题：**谁回来收这活？** 有主的东西才谈得上"停"；无主的东西只有
`del_bash` 停得住，而本回合被换掉时没有任何人回来。

---

## 1. 进程：三种 ownership

同一个 `bash` 工具，`background` / `detached` 改的不是"等不等"，是**所有权**。

| 模式 | 谁拥有 | 输出去哪 | 谁结束它 | 本回合 `cancel` 时 |
|---|---|---|---|---|
| 默认（前台） | 本回合 | 工具结果，流式 | 命令结束 / 回合取消（连进程树一起杀） | 被中断，shell 换新 |
| `background` | 那个 shell | shell 的未读缓冲，`check_bash` 取走 | `del_bash` 随 shell 一起消失 | 看取消点：卡在那条 shell 的前台命令里就一起被杀，否则活着 |
| `detached` | **没人** | 只有它自己的日志文件 | **只有 `del_bash`** | **活着** |

实测（本会话）：

- `detached` 起了个每 1s 追加一行的循环 → `del_bash` 之后行数还在涨（13→15）；
  `list_bash` 把它单列在 `detached[]` 里，带 `pid` / `log` / `running`。
- `del_bash --name_regex '^demo-detached$'` → 行数 15→15 停住，`detached[]` 变空。
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
新服务端启动时 `adopt()`，但**绝不**接管 pid 等于自己或自己祖先的行——否则 `del_bash`
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

## 3. 时间：等待可以被两件事打断，命令都不会被杀

前台 `bash` 的等待有两个触发源，**两个都只结束等待，不结束命令**：

| 触发 | 回执开头 | 谁等的 |
|---|---|---|
| `timeout_ms` 到（默认 120s） | `still running after <n>ms, so the wait ended` | 你自己设的期限 |
| **有人给本会话发消息** | `a message arrived for this session, so the wait ended and it was handed to the background` | 不是你能预知的 |

第二种是 2026-10-24 加的，也是更常见的那个。实测：先让一个 detached 作业 5 秒后
`send_message` 给我，同时前台跑 `sleep 40` —— 调用 **3 秒就返回了**，40 秒的命令自己跑满，
输出（`done after 40s`）之后作为 inbox 消息投回来。

所以：**你不需要为了"能被打断"去开短 timeout**。它就是会断。反过来说，
长命令被交给后台是**正常现象**，不是故障——不要因为它提前返回就以为命令失败了。

两种情况下，**同名 shell 都换成了新 spawn 的一个，cwd 回到会话目录**。所以之后不要假设
`cd` 还在；长任务的前缀请写成绝对路径或一句 `cd` 重来。

由此：

- 等待不是预算，也不是 kill。要真停就在前台等它被杀（取消），或 `del_bash`（detached）。
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

**它现在还能叫醒你**（2026-10-24 起）：前台 `bash` 正等一条长命令时，消息一到，等待就结束、
命令交给后台继续跑（见 §3）。所以这不是"留言"，是一条**打断**——`sleep 40` 实测 3 秒返回。
两面的用法：

- 想让长命令早点放手 → 从别处发一条消息，不用干等。
- 小心：你自己排队发的消息也会打断自己正在等的东西，别在关键写入中途自己插自己。

规矩：

- 它是**用户消息**，会一直留在 transcript 里，还会被压缩进 summary。不要当 printf 用。
- 只能用在这一处合法位置：`function_call` 和它的 `function_call_output` 之间插 user 消息
  会被 provider 整单拒绝，所以注入点在 step 边界。
- 消息自己也是 pending 行，`cancel` 前后都能活下来 —— 这是跨重启留意图最省事的办法。

---

## 6. 换掉自己（self-evolution）

完整流程在 **`evolve` skill**（`.agents/skills/evolve/SKILL.md`），脚本是
`.agents/skills/evolve/evolve.sh`。这里只留几条容易忘的：

```bash
S=.agents/skills/evolve/evolve.sh
$S stage        # 测试 + 构建到 .dev/target + 暂存 + 生成 launcher
# 用 bash --detached 启动它打印的 launcher
$S swap <stamp> # 守卫通过才 cancel
```

1. **先起继任者，再 cancel**。反过来一旦新二进制起不来，home 就空着，没人 resume 本回合。
2. **不要写交接笔记。** resume 会重放完整 transcript（含每个工具调用和结果），笔记不会
   多提供任何信息；02:24 那次我写了笔记，结果 pending 行丢了、根本没人被唤醒。
   该做的是**重新测量外部状态**：`server.lock`、端口、`pending_tasks`、残留 detached。
3. **验证看继任者自己的日志**，不看 `server.lock` 变了没：

   ```bash
   sed 's/\x1b\[[0-9;]*m//g' ~/.gongga/detached/<最新>.log | grep -E "replaying a turn|resumed=true"
   ```

   两行都在、时间戳几乎相同，才算接上了。
4. **Windows 上 `exec` 不保留 pid**（实测：launcher 是 bash，exe 成了它的子进程）。
   所以 `server.lock` 的 pid ≠ `launch.pid` 是**正常**的，别据此判断换手失败。

### 改服务端代码时的两个 Windows 坑

1. **`拒绝访问 (os error 5)`**：`target/` 里的 `.exe` 正被跑着，`cargo build` 写不进去。
   如果你为了测试起过一个服务端，先关掉它再编。
2. **别信"私有 target 目录会让 `cli_path` 测试失败"这句话。** 实测
   `CARGO_TARGET_DIR=.dev/target-test1` 下那条测试**是通过的**（`cli_path()` 有三层兜底）。
   看到"某条测试会失败"这类断言，先自己跑一遍再转述——包括本文档里的。

日常开发换二进制用 `scripts/dev.sh`（把 exe 复制出 `target/` 再起，避免锁）。

---

### 改服务端又不影响自己：隔离一套环境

要验证服务端改动（新端点、新工具、新 roster 字段），**不要用你自己正在跑的 home 和 roster**。
一套隔离环境只多三行：

```bash
# 1. 自己的 roster，指向自己的 fake 端点
cat > .dev/v/llm.toml <<'EOF'
[[provider]]
id = "fake"
  [[provider.endpoint]]
  api = "openai-responses"
  base_url = "http://127.0.0.1:8899/v1"
  ...
EOF

# 2. 自己的 home + 端口；GONGGA_LLM_CATALOG 才是 roster 的入口
GONGGA_LLM_CATALOG=$PWD/.dev/v/llm.toml GONGGA_KEY=sk-fake \
  ./.dev/target-test1/debug/gongga-server.exe --home .dev/v/srv --bind 127.0.0.1:8900

# 3. 客户端显式指过去
./target/debug/gongga.exe --server 127.0.0.1:8900 --key sk-fake set_session --session s --llm fake/fake
```

三个坑：

- **`HOME=` 不管用**：roster 走 `GONGGA_LLM_CATALOG`，`dirs::home_dir()` 在 Windows 上
  不读 `HOME`。设了 `HOME` 以为隔离了，实际还是读你真实的 `~/.agents/llm.toml`。
- **roster 是 `OnceLock`，进程启动读一次**。改了 roster 要重启服务端才生效。
- 用 `scripts/fake_llm.py` 当端点，`FAKE_LLM_DUMP=<path>` 会把**真实请求体**逐行写下来——
  想看"服务端到底发了什么"，这是唯一可信的办法，别靠读代码推测。

验完记得收尾：关掉测试服务端（否则 `target/` 里的 exe 锁着，见上面的 `os error 5`），
删掉 `.dev/v/`。

---

## 7. 单向门：做之前先看范围

| 动作 | 不可逆的部分 |
|---|---|
| `del_session --session <n>` | 删 transcript、删 pending，**并停掉该会话的 detached 作业**（`cancel` 不这么干） |
| 从外面杀服务端 | 本回合没人接，等于丢了一次未落盘的判断 |
| 没有继任者就 `cancel` | home 空着，直到有人手动起服务端才 `resume_pending` |
| `del_bash` | 进程没了；日志文件还在，但状态没了 |
| `del_bash` | 那个 shell 的 cwd / export / 函数全清零（后台作业一起死） |

---

## 8. 看自己

| 想知道 | 怎么看 |
|---|---|
| 我有几个 shell、谁还有没读的输出、有哪些 detached | `list_bash`（`unread_bytes`、`background_jobs`、`detached[]`） |
| 后台作业跑到哪了 | `check_bash --name <shell>`，**读即清空**，同一段不会给两次 |
| 我刚才到底做了什么、上一条命令的原文 | `get_messages --session <s> --seq <a>:<b>`（1 基、两端含，`-1` 最新一条，如 `-5:` 取最后五条） |
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
  用 `del_bash`。
- **用 `send_message` 记日志。** 它会永久留在 transcript 里并进入 summary，而且**会打断
  你正在等的命令**（见 §5）。
- **信 `.agents/notes` 或本文档里的"某条测试会失败"而不自己跑一遍。** 已经踩过一次：
  有记录说私有 target 目录会让 `cli_path` 测试失败，实测是通过的。
- **以为写了 skill 文件就等于能被发现。** 发现靠提示词里那句索引；索引不在（旧服务端、
  cwd 不在仓库里），文件就是死的。先跑一遍索引命令再指望别人读到。

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
| 三种 bash 模式的存活语义（含 `del_bash` 后 detached 继续、`del_bash` 停住） | 本会话实测 |
| 超时只结束等待、输出随后回投、shell 被替换 | 本会话实测 |
| **消息打断等待**、命令不被杀、输出随后回投 | 本会话实测（`sleep 40` / 5 秒后发消息 → 3 秒返回，命令跑满 40s 并回投） |
| 忙时 `send_message` 在下一个 step 边界投递 | 本会话实测 |
| `cancel` 保留 pending、`interrupt_session` 删 pending、`del_session` 连带停 detached | 读 `crates/server/src/commands.rs` |
| resume 不带任何 instruction | 读 `agent.rs::turn_inner`（`user_text: None` 不写行） |
| 压缩 70% / transcript 不改写 / `summarised_upto` | 读 `agent.rs`、`store.rs` |
| evolve 的四条纪律与 swap 守卫 | 读 `.agents/skills/evolve/evolve.sh` + `status` 实跑 |
| `evolve.sh swap` 的完整换手 | **实跑 9 次**，其中 02:24 那次**丢了回合**（pending 行被 `resume_pending` 的收尾无条件删除），修好后 3 次全部 resume 成功 |
| swap 后验证行为而非 pid | 本会话实测：新二进制里"消息打断等待"只有真跑一条长命令才验得出，`server.lock` 变了不证明任何事 |
| 只读 sqlite 自查 | 本会话实测 |

---

## 12. 技能本身是怎么被发现的

服务端的系统提示是一段**静态字符串**（`crates/server/src/config.rs::default_instructions`），
它不认识 skill，也不读 `AGENTS.md`。所以"懒发现"只能靠提示词里那句**索引指令**：

```bash
grep -H -m3 -E '^(name|description):' .agents/skills/*/SKILL.md
```

每个 skill 两行：路径 + name，路径 + description。**这是全部成本**——先只读这个，命中才读正文。

要点：

- **别把 skill 列表写进 Rust 字符串**。加一个 skill 就要重新编译 + 换服务端，太重。提示词里只放
  *怎么列*，skill 本身是文件。
- 正文要能独立成立：读它的人只有任务和这份文件，没有你的上下文。
- 附带脚本就写在 SKILL.md 旁边，让自己直接跑，而不是每次重敲命令。
- 路径相对仓库根；会话 cwd 不在仓库里就没有 skill，别假装有。

历史：这个机制是 2026-10-24 加的。之前 skill 文件存在但**没人会看见**——包括本文件第一版。
