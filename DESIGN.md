# Gongga Harness — 设计说明

依据 `SPEC.md` 实现，并记录了实现过程中做出的判断。其中标注 **【待确认】** 的是偏离或有歧义的地方，需要你拍板。

## 1. 结构

```text
Cargo.toml                  workspace，resolver 3，edition 2024
crates/protocol/  gongga-protocol   别名 protocol —— 帧、可靠 UDP 通道、消息词汇
crates/server/    gongga-server     别名 server
crates/cli/       gongga-cli        别名 cli（二进制名 gongga）—— 命令层
crates/tui/       gongga-tui        别名 tui —— 纯终端 UI，不认识命令
```

命名沿用你 `whiting` 项目的约定：目录不带前缀，`[workspace.dependencies]` 里用
`package = "gongga-xxx"` 做别名。

依赖方向是单向的：`tui` 不依赖 `protocol`，`cli` 同时依赖两者。

**`tui` 的边界**：SPEC 说「tui: transparent cli wrapper, do not know cmd」。所以 tui 只认识
「行」「候选词」「按键」，不认识命令、不认识协议：

```rust
pub trait Completer { fn complete(&self, line: &str, cursor: usize) -> Vec<Candidate>; }
pub enum UiEvent { Submit(String), Cancel, Quit }
pub enum LineKind { System, User, Assistant, Reasoning, ToolCall, ToolOutput, ToolError, Result, Error }
```

补全候选由 `cli` 注入（它持有服务端下发的命令目录），tui 只负责弹窗和插入。
验证方式：给 tui 换一个 completer 它照样工作，说明耦合真的断开了。

## 2. 传输层：可靠 UDP

按你的选择走「跨机 + 自研轻量可靠层」。没有用 QUIC（强制 TLS，跨机自签证书很麻烦），
也没有用 KCP 绑定（API 是回调式的、非 async）。自研约 500 行。

### 帧头（26 字节，定长）

```text
 0.. 2  magic    u16  0x4747
 2.. 3  version  u8   1
 3.. 4  flags    u8   SYN|ACK|FIN|RST|DATA|PING
 4.. 8  conn_id  u32  服务端分配
 8..12  seq      u32  包序号（ARQ 单位）
12..16  ack      u32  累积确认
16..20  msg_id   u32  本分片所属逻辑消息
20..22  frag_idx u16
22..24  frag_cnt u16
24..26  window   u16  接收窗口（包数）
```

默认 MTU 1200（含头），即 1174 字节有效载荷。1200 是跨公网的保守值，避免 IP 分片。

### 恢复策略

- **累积 ACK**，能捎带就捎带，否则延迟 20ms 单独发；乱序到达立即 ACK。
- **RTO** 用 RFC 6298 的 SRTT/RTTVAR 估算，按包独立退避（`rto << min(retransmits,5)`），
  钳在 [50ms, 5s]。按包而非按通道退避是关键：一个倒霉分片不会把整个窗口的定时器拖长。
- **快重传**：3 个重复 ACK 触发。
- **每 tick 最多补发 8 个**过期包 —— 一次只补一个的话，局域网外的突发丢包会拖成秒级停顿。
- **按序交付**：接收端缓冲乱序包，只有连续了才往上交；因此同一消息的分片必然连续到达，
  重组就是简单 append，不需要按 msg_id 做乱序重组表。
- 心跳 2s（保 NAT 映射），空闲 60s 判定连接死亡。

`tests/loopback.rs` 用真实 UDP socket 验证：200KB 消息分片往返、25% 丢包下恢复、
多条消息保持顺序。丢包恢复从最初的 25s 优化到 <0.4s。

### 服务端多路复用

单个 socket 服务所有客户端。`recv_from` 收到帧后按 peer 地址分发给对应连接的 inbox；
未知地址只有 SYN 能建连。**没有用 per-peer socket + SO_REUSEADDR**，因为在 Windows 上
那是地址抢占行为，不可靠。

### 连接密钥（SPEC: UDP + key）

握手顺序是**客户端先说话**：client 发 `Hello{client, key}`，服务端校验通过才回 `Hello`
（携带命令目录和会话列表）；不通过回 `Denied{reason}` 并关闭。这样未鉴权的 peer 连目录都拿不到。

- 密钥默认从 `~/.gongga/config.toml` 的 `[server] key` 读；**没有就生成一个 128 位随机 hex
  并写回配置文件**，而不是开放启动。也可 `--key` / `GONGGA_KEY` 覆盖。
- 比较是**全字节折叠**的（`key_matches`），不短路：密钥明文走 UDP，短路比较会让攻击者用
  响应时间逐字节试出正确前缀。
- 握手超时 10s。

**【待确认】** 现在止步于「明文密钥 + 鉴权」，没有加密：同一网段仍能**看到**流量内容。
要加密得上 DTLS 或换 QUIC。需要吗？

## 3. 消息与事件

JSON over 上述通道（可读性 > 字节数；这是人机速的事件流）。

```text
ClientMessage: Hello{client,key} | Command{id,name,args} | Cancel{session} | Bye
ServerMessage: Hello{protocol,server,commands,sessions} | Denied{reason} | Event(event)   # 相邻标签 {t,v}
Event:         Cmd | CmdResult | Response | ToolAnswer | Sessions | Status | Log
```

`ServerMessage` 用**相邻标签**（`tag="t", content="v"`）而不是内部标签：
`Event` 自己也带 `t` 判别符，内部标签嵌套会序列化出两个 `t`，客户端直接反序列化失败。
这个坑在第一次端到端冒烟时踩到了。

**sticky 参数不在协议里**。SPEC 把它放在 UI 一节（`persistant_cmd_arguments[{name,value}]`），
所以它是纯客户端概念：CLI 保存在自己的 `~/.gongga/cli.toml` 里，发送前补齐，服务端完全不知道。
早先版本让服务端存 `cmd_args` 表、在 dispatch 前补齐 —— 已按 SPEC 删掉。

`response{session, reasoning, toolcall, output}` 的实现：

```rust
ResponseEvent { session: String, turn: u64, part: ResponsePart, text: String, done: bool }
enum ResponsePart { Reasoning, ToolCall, Output }
```

即 **session 是归属字段，reasoning/toolcall/output 是三条可交错的子流**，每个 delta 带
`part` 和结束标志。**【待确认】** 这是我对你 SPEC 里那行记法的读法，另一种可能是
`session` 也是一条子流。

`turn` 从 1 递增，客户端可据此丢弃 cancel 之后到达的陈旧 delta。

## 4. 服务端

### 一个 home 只允许一个服务端

启动时在 home 下抢占 `server.lock`（内容就是自己的 pid，`create_new` 是原子的，同时抢只有
一个赢）。第二个服务端会直接退出，并告诉你谁占着：

```
Error: another gongga server is already running on this home (pid 43516,
home C:\Users\17124\.gongga); stop it first, or delete ...\server.lock ...
```

为什么要这个：UDP `bind` 只能挡住**同端口**的第二个服务端；换个 `--bind` 指向同一个 home
就能绕过，而两个服务端共用一份 SQLite，会各自 resume 同一条 pending、压同一份上下文、同时
驱动同一个会话 —— 这正是「压缩为啥会并发」那次的形态（不过那次并发的不是两个服务端，见
§8 第 15 条）。

被强杀的进程来不及删锁，所以锁里存 pid：启动时问一次它还活着吗（Windows 用 `tasklist`，
Unix 用 `kill -0`），死了就当垃圾接管。优雅退出（`cancel` 命令）走 `Drop` 正常删锁。

### 冲突的操作排队，不抢

一个会话上的操作共享同一份 transcript，所以会互相踩的都得**排队**，而不是靠运气。排队的
是 `SessionHandle` 上的一把 FIFO 门（`tokio::sync::Mutex`，先到先得）：

| 操作 | 为什么和别人冲突 |
|---|---|
| turn | 两个 turn 会各自量一遍上下文、各自判定超预算、把同一份内容压两遍 |
| `del_session` | 会把 transcript 从正在写它的 turn 脚下抽走 |
| 同一 shell 的 `bash` | 一条命令的 `cd` 会改掉下一条的工作目录（本来就有 per-shell 锁） |
| detached 注册表 | 已有全局锁 |
| `Store` | 单条 SQL 有连接锁，但跨调用的读-改-写靠上面这些排队才成立 |

拿门必须在 `new_turn()` **之前**：`new_turn` 的语义是「取消正在跑的那个」，一个还没被放行的
turn 调用它，就会杀掉自己本该排在后面的那个 turn。所以现在「发消息」的意思从"打断"变成
了"排队"—— 想打断是 Ctrl+X 的事。

排队中的 turn 有两种「不用跑了」的情况，都会在放行时立刻放弃，不留任何痕迹：

- **等待期间被 Ctrl+X**：`cancel` 递增一个计数，turn 拿门前后各读一次，变了就走人。否则会
  出现"我明明取消了，它怎么又开始跑"。
- **等的是一次 `del_session`**：会话已被删除，写下去等于把用户要求删掉的行复活。

等待本身会打日志（`turn waited for the session to be free` / `del_session waiting ...`），
否则一个排了三分钟的 `del_session` 看起来和服务端卡死没有区别。

顺带修掉一个排队暴露出来的 bug：`send_message` 的收尾原本按**会话**清 pending 行，第一个
turn 结束时会把还在排队的第二条消息的 pending 一起删掉 —— 那条消息既不在 transcript 里
（要等它自己那个 turn 才写入），也不在 pending 里，服务端一崩就彻底消失。现在 `add_pending_task`
返回行 id，收尾只删自己那一行（`store::tests::finishing_one_pending_task_leaves_the_others`）。

### 存储（SQLite）

`sessions / messages / tool_calls / events / pending_tasks`，WAL 模式，外键级联。

「structured for agentic retrieval/mutation」理解为**不要存成一大坨 JSON blob**：
每条消息是 typed 行（`kind` ∈ text|reasoning|tool_call|tool_result），可直接 `SELECT`
某会话的全部失败工具调用、或改写单条消息，而不必反序列化整份文档。

**没有 `meta` 表，也没有「当前会话」。** 会话要么被点名，要么命令就报错；默认值只可能
来自客户端的 `persistant_cmd_argument`，它在命令上线前就被填好。服务端不保存任何
「你上次用的是哪个会话」这类隐藏状态。

`pending_tasks` 是未完成的活：一个 turn **开始前**写行、**结束后**删行，所以崩溃留下的痕迹
和干净退出相反 —— 表里有啥，下次启动就欠啥。

两种 cancel 的区别只在这一行上：

| 谁 | 语义 | pending 行 |
|---|---|---|
| Ctrl+X / `ClientMessage::Cancel` | 「别干了，丢掉」 | 删（turn 收尾时清） |
| 服务端命令 `cancel` | 「停下来，剩下的下次接着干」 | **留**（`cancel_and_preserve`） |

第二行以前是坏的：`cancel` 只置了取消标志，turn 自己收尾时照清不误，于是回执里
「N pending task(s) will be picked up」是句空话 —— 只有被 `taskkill` 硬杀才真的留得下。
现在 `cancel` 走 `cancel_and_preserve()`，turn 收尾前先看这个标志。

`detached_jobs` 是**跨服务端存活的进程**：`bash --detached` 启动一个就写一行
（session / shell / job / pid / cmd / log），停掉才删。它是 detached 语义的地基：
进程不归服务端所有，所以「谁还活着」这件事只能靠数据库交接给下一次启动。

**【待确认】** 是否需要进一步给 agent 暴露一个 DB 查询/改写工具？目前没有。

连接放在 `Arc<Mutex<Connection>>` 里同步调用。这里的语句都是本地亚毫秒写；若哪天不成立，
修法是 `spawn_blocking`，不是改架构。

### Agent loop

一个 *step* = 一次模型请求 + 它调用的工具；一个 *turn* = 若干 step，直到模型不再要工具、
步数预算（默认 64）耗尽、或被取消。每一步之后 transcript 已经落盘，所以中途崩掉留下的是
可重放的会话而不是空洞。

- 取消：per-session `CancellationToken`，新 turn 会取消旧的。
- 工具顺序执行（并行对持久 shell 没意义）。
- 失败/取消通过 `Event::Status{status}` 和 `Event::Log` 上报，不向调用方返回错误。

#### 上下文体积：两道闸

一个 step 会把整个 transcript 重发一遍，所以「一次性输出」的实际代价是**之后每一步都再付一次**。
实测（`test1`，79 个工具调用）：单次请求从 1KB 长到 414KB，整个回合累计发了 **17.3M 字符**，
最后一次请求已接近模型窗口。两道闸分别砍两头的来源：

**闸一：工具输出上限（默认 8000 字符，`tools.bash.max_output_chars`）**

前台 `bash` 和 `check_bash` 的返回超过阈值时，只把**头 60% + 尾 40%** 交给模型，
完整输出写到 `~/.gongga/outputs/<session>-<shell>-<call_id>-<时间戳>.out`（路径用 MSYS 写法，
模型可以直接 `sed -n 'START,ENDp'` 取片段）。落库的是截断版本 —— 关键就在这里：
UI 上流式看到的仍是完整输出，但进 transcript 的不会反复计费。
实测：一次 22KB 的 `cat` 变成 2004 字符 + 一行落盘提示。

之所以不是「截断丢弃」而是「落盘 + 指路」：信息没丢，模型想看哪一段还能看，
只是不再把整段塞进每个后续请求。设为 0 可关掉。

**闸二：摘要式压缩（默认超过 12 万字符触发，`agent.compress_above_chars`）**

每个 step **之前**检查一次：若本次请求要带的字符数超阈值，就把较早的部分交给模型压成
一条 handover note。

- **非破坏性**：`sessions.summary` + `sessions.summarised_upto` 只标记「摘要覆盖到哪里」，
  一行消息都不删 —— transcript 是记录不是缓存，删了就没了，且 UI 仍能回放全部历史。
  请求 = 一条 user 消息（摘要前缀）+ `seq > summarised_upto` 的消息。
- **切点只放在能独立站立的位置**：`function_call_output` 前面必须有 `function_call`，
  否则 API 直接拒；reasoning 前面被挖空也会白占空间。所以切点只会落在 `text` 或 `tool_call`
  之前，最近 N 条（默认 30）原样保留。若整个窗口都找不到安全位置，这次就不压。
- 摘要请求本身**不带工具、不流式**，上限 2000 token；失败只记日志，turn 照常继续，下一步再试。
- 已有摘要时会作为「在此之前」一起喂进去，链式压缩不会丢掉更早的结论。

实测（阈值压到 3000 便于触发）：一次真实会话在第 51 条消息处压缩，产出 2318 字符摘要，
保留了用户请求、已确认结论、待办；之后会话照常继续，模型没有报格式错误。

**【待确认】** 压缩后保留区间里的 reasoning item 引用的前文变了，provider 可能解密失败
（实测没有报错，但也没有直接证据表明它仍然消费了这些块）。若发现模型「失忆」，
可以把切点放宽到「只在 user 消息前切」。

### LLM（OpenAI Responses API）

默认 provider 是 **minimax-cn / MiniMax-M3.1-Flash**（取自 DSH 的 `models.dev` 缓存）：

| 项 | 值 |
|---|---|
| `base_url` | `https://api.minimax.cn/v1` |
| `model` | `MiniMax-M3.1-Flash` |
| `api_key_env` | `MINIMAX_CN_API_KEY` |
| key 来源 | 环境变量 → **`~/.dsh/.credentials.yaml` 的 `refs:` 块** |

已实测：`https://api.minimax.cn/v1/responses` 接受标准 Responses API 请求，无需代理。
（该 host 的 `/anthropic/v1` 是 Anthropic 兼容端点，DSH 的 models.dev 记录指向它；
我们按 SPEC 走 Responses，所以用 `/v1`。）

`POST {base_url}/responses`，`stream: true`，手写 SSE 解析（不引 SSE crate，因为要能
在参数未完整时就往外吐 tool-call delta）。

- 默认 `store: false`：每次请求带完整 transcript，不依赖服务端状态。
- reasoning 回放需要 `encrypted_content`，所以请求 `include: ["reasoning.encrypted_content"]`；
  拿不到加密块时 reasoning 项会从历史里丢弃，而不是发一个 API 会拒的格式。
- **回传的加密块确实被消费**（真实 provider 实测，2026-10-22，MiniMax-M3.1-Flash）：
  第一轮算 `17*23`（拿到 259 字符 blob）；第二轮 input **只**放一个 reasoning item +
  `"and what is that doubled?"`，不带任何第一轮的问答文字 ——
  - 带真 blob：答 `17 × 23 = 391. Doubled: 782.`，且**不再产生 reasoning**（16 output token）
  - 带垃圾 blob：`"Which value do you mean by 'that'?"`，重新思考 83 reasoning token
  - 不带 reasoning：同样答不上来，重新思考 51 token

  即 blob 里编码了上一轮的推理内容，服务端解密并接着用；格式不对就静默丢弃回到冷启动。
  所以这一项是 interleaved thinking 的命脉，**不能为了省 token 砍掉**。
- **可读的 `summary` 从 delta 累积**：MiniMax 在 `output_item.done` 上给的 `summary` 数组
  是空的（它发的是 `response.reasoning_text.delta`，不是 summary delta），所以思考文本
  只存在于 delta 流里。delta 按 `item_id` 累积进 `ItemMeta::text`（没有对应条目就建一个，
  因为并非每个 provider 都先发 `output_item.added`），done 时 provider 的 summary 优先、
  否则用累积值。不这么做落库的 reasoning 就全是空摘要。
  实测：修复前 summary 恒为 0 字符，修复后 395 字符。
- summary **只落库不回传**：模型靠 encrypted blob 恢复推理状态，把摘要也塞进 `input[]`
  等于每句话存两份。summary 是给 UI 重连时回放看的。
- `history_item_to_input` 负责把存储的行还原成 Responses API 的 input item 形状。

### 工具（SPEC: bash / check_bash / list_bash / del_bash，外加 kill_bash）

Shell **按 `session_name/bash_name` 建键**，所以一个会话持有的是一组常驻 shell，不是单个：

- `default` 在会话物化时就存在。
- `bash` 带上别的 `name` 时**按需 spawn**，起始目录是会话目录。
- `del_bash{name}` 丢弃一个 shell；下次再点名会重新 spawn，因此 del 之后 bash 依然可用
  （只是状态清零）。这正好对应「shell 被搞脏了就换一个」。
- 每个 shell 是常驻 Git Bash（`--norc --noprofile`，stdin 管道驱动），cwd / export /
  shell 函数跨调用保持。
- 命令结束后向 **stdout 和 stderr 各打一个带退出码的哨兵**，两个流可以独立收尾又都能拿到状态码。
  哨兵是每 shell 随机的，避免输出里恰好出现相同文本导致截断。
- 超时（默认 120s）后 **杀掉并重建该 shell**，回到上一个已知目录 —— 半死的 shell 会污染之后
  所有命令，所以重建是唯一安全的选择。detached 作业不属于这个 shell，不受影响。
- 每个 shell 启动时注入 `GONGGA_SERVER` / `GONGGA_HOME` / `GONGGA_CLI`（重建时同样注入）。
  这是**模型驱动自己的唯一通道**：`bash` 跑在服务端的子进程里，不告诉它这些，它就既不知道
  服务端的地址、也不知道客户端在哪，想取消或重启只剩从外面 `taskkill` 这一条路。
  三个变量正好是 CLI 的两个 fallback 加一个可执行文件路径，所以 `$GONGGA_CLI cancel`、
  `$GONGGA_CLI send_message --message hi --session main` 都不需要任何额外参数。

`bash{cmd, name, background, detached}` 的三种跑法：

| 模式 | 是否等待 | 输出去哪 | 谁结束它 |
|---|---|---|---|
| 默认（前台） | 等，流式回传 | 直接作为工具结果 | turn 取消即中断 |
| `background` | 不等 | tail 到 shell 的**未读缓冲区**，由 `check_bash` 取走 | `del_bash` 随 shell 一起消失 |
| `detached` | 不等 | 写进自己的日志文件，**不回收** | 只有 `kill_bash`；`del_bash` / `cancel` / 服务端退出都不碰它 |

- `background` 的实现：shell 把作业重定向到一个 scratch 文件（路径按 MSYS 写法转换，
  `C:\Temp\x` → `/c/Temp/x`），服务端任务每 150ms 读一次增量，追加进未读缓冲，
  看到结束哨兵就收尾删文件。`check_bash` 取走即清空，同一段输出不会返回两次。
- `detached` 的实现**不在常驻 shell 里跑**：服务端直接 spawn 一个 `bash -c "<cmd>"`，
  继承 shell 的 cwd（但不继承 export / shell 函数），自己的进程组，
  `kill_on_drop(false)`。
  为什么这么绕：先试过 `nohup ... &` + 记住 `$!` + `kill <pid>`，结果杀不掉真正干活的进程 ——
  MSYS 的 pid 不是 Windows pid（`taskkill` 认不出来），`kill -- -PGID` 在 MSYS 里
  「No such process」，而 `$!` 拿到的只是包裹用的子 shell，杀掉它 `sleep` 照样跑完。
  另一个坑：pid 会复用，所以 kill 前先 `try_wait`，进程已经退出就**绝不**再拿它的 pid
  去杀 —— 否则会杀到刚巧复用这个号的别的进程。

#### detached 到底是为了什么：跨 server 存活

`detached` 不是「后台跑」，是**服务端放弃所有权**。典型用法就是你说的那个：
一个 agent 想重启它正跑在上面的服务端 ——

1. `bash{cmd:"gongga-server --bind ... ", detached:true}` 拉起替身；
2. 然后 `cancel` 自己；
3. 替身活下来接管，客户端改连新端口即可。

所以三件事必须同时成立，缺一个这个用法就断了：

| 要求 | 怎么保证 |
|---|---|
| 调用立刻返回，服务端没被卡住 | 不等子进程，工具结果里直接给 pid 和日志路径 |
| 服务端 `cancel` / 退出时它不死 | `kill_on_drop(false)` + 独立进程组；`cancel` 明确**不**杀 detached（只报数量） |
| 重启后仍然叫得停 | pid 写进 `detached_jobs`；新服务端启动时 `adopt()` 重新接管 |

`adopt()` 的规则：进程还在 → 接管（job 号沿用上一任给的，不复用）；进程没了 → 删行
（那是干完的活，留着只会让列表说谎）；**pid 等于自己 → 一定不接管**，否则
`kill_bash` 就成了服务端自杀按钮。

代价说清楚：接管靠的是 pid 是否存活，没法证明它还是原来那个进程，理论上存在 pid 复用
窗口。这个窗口很窄，而且后果被 `kill_bash`（必须显式调用）框住了。

detached 作业归**会话**所有、不归 shell：它们活在一个 `DetachedRegistry` 里
（session → jobs），`list_bash` 会把它们单独列出来（含上一任留下的），
`kill_bash{name, job?}` 是唯一结束它们的方式（不给 `job` 就是停掉该 shell 的全部）。
`del_bash` 不杀它们，但会**明确告诉模型还有几个在跑**，免得模型以为丢 shell 就等于停了。
`del_session` 则相反：删除会话意味着「它起的活也不要了」，会一并停掉。

按你的选择**不做人工审批**。**【待确认】** 完全没有护栏，是否需要至少一份危险命令黑名单
（如 `rm -rf /`、写系统目录）？

### 命令（SPEC 的 4 条服务端命令 + 2 条客户端命令）

服务端：`send_message --message <m> --session <s>`、`set_session --name <n> [--cwd]`、
`del_session --name <n>`、`cancel`。
- `set_session` 同时承担创建：命名是唯一得到会话的方式，拒绝创建会让它没用。
  **它不再设置任何「当前会话」** —— 想要默认值就用客户端的 sticky `session`。
- `send_message --session` 是**必填**的。缺了就在客户端被拒（目录里标了 required），
  服务端也再查一次，不给自己留隐式落点。
- `cancel` 是 SPEC 那条「not instantly」的停服：先取消所有在跑的 turn、把没干完的活留在
  `pending_tasks` 里，等 250ms 让「正在取消」的回执发出去，再让监听循环退出，并给在跑的
  turn 最多 30s 落盘。下次启动 `resume_pending` 自动重放。
  **它不杀 detached 作业**（回执里会报「N 个还在跑，用 `kill_bash` 停」）——
  那正是 detached 存在的理由，见上一节。

客户端（本地执行，不上线）：`set_persistant_cmd_argument --name --value`、
`del_persistant_cmd_argument --name`。

## 5. CLI（命令层）

`cli` 持有两样东西：服务端下发的命令目录，和本地的 sticky 参数。它把输入行翻译成
`Submission::{Send, Local, Rejected}`，再通过 `Sink` trait 交给前端渲染 ——
TUI 是一种实现（带样式的行），非交互模式是另一种（什么都不渲染，只把事件打成 JSON）。

- **补全目录 = 服务端目录 + 客户端目录**。三个可补全位置：命令名、`--` 后的参数名、
  声明了来源的参数值（`Sessions` / `Static`）。`del_persistant_cmd_argument --name`
  的候选来自当前 sticky 参数名，随 sticky 变化重建。
- **输入行只有一种语法：命令。** 首 token 命中已知命令就按命令解析；不命中就拒绝并提示
  `send_message --message <text> --session <name>`。
  曾经有一版「裸文本 = 发给 sticky `session` 的消息」的简写，已删除：它让同一行输入
  的含义取决于一份看不见的状态，也正是 UI 看起来「有当前会话」的根源。
- sticky `session` 仍然生效，但只做 SPEC 说的那件事：**补齐命令里没写的参数**
  （`send_message --message hi` → 补 `--session`）。它不改变一行的含义。

TUI 和 CLI **共用这一套模型**，TUI 不持有任何额外状态：它只是 `Sink` 的一种实现。
状态条只报服务端事实（model、会话数量），**没有「当前会话」这一项**；sticky 参数单独一行
显示，那是用户显式设的 persistant argument，不是 UI 的模式。
Ctrl+X（`cancel`）是唯一会推断会话的地方，它取 sticky `session`，取不到就提示怎么设。

TUI 布局：头部状态条 / 事件流 / sticky 参数行 / 输入行 / 帮助行，补全弹窗浮在输入行上方。
滚动：为算准「底部」位置，日志是**手工按显示宽度换行**后取可视切片，而不是交给
Paragraph 自动换行（那样拿不到换行后的总行数）。`scroll` 的含义是「距底部多少行」（0 = 跟着
最新一行），它的上界 `max_scroll` 由**渲染回写**——只有渲染知道换行后的总行数和窗口高度；
滚动键和渲染各 clamp 一次，所以行被删掉或窗口变高时视图不会停在空白处。

翻事件历史的键位：↑↓ 归命令历史（shell 习惯，也是人手先试的那两个），←→ 是输入行光标，
所以翻页落在 **Ctrl+↑↓（一页）/ Shift+↑↓（一行）** 上，PgUp/PgDn 同样翻页但在笔记本上
藏在 Fn 后面。**鼠标滚轮一次 3 行、按住 Shift 一次 1 行**：为此进 alternate screen 时必须
开 mouse capture（退出时关掉），否则滚轮事件根本到不了进程，终端自己留着也没处滚。

### 模型请求的超时与重试

一个「连上了但不说话」的 provider 曾经能把 turn 永久挂住：没有错误、没有 EOF、没有 socket 可
查，谁也叫不醒。所以请求被两头夹住，并且会自己再来一次：

- `request_timeout_ms`（默认 180s）：单个请求的总时长上限，流式也算在内。
- `idle_timeout_ms`（默认 60s）：SSE 两个 chunk 之间的静默上限 —— **连接开着但不发数据就是坏
  掉**，这正是卡死那次的形态。
- `max_retries`（默认 3）/ `retry_base_ms`（默认 1000）：失败、超时、或**完成了但一个字都没有**
  的空响应，都会退避重试，延迟翻倍（1s → 2s → 4s，封顶 30s）。压缩（summarise）那条请求同样受保护。
- 只有「本次尝试一个字都没说」才允许重试：已经说过话再问一次，会给出第二个互相矛盾的答案。
- 重试会发一条 warn 到 TUI（`retry 1/3 in 1s`），所以慢的时候看得到是在重试，而不是在思考。

`GONGGA_LLM_*` 环境变量可覆盖全部四项。

**卡住不等于网络问题。** 2026-10 那次「agent 卡住」排查下来是两件事叠在一起，都不在网络上：

1. 压缩提示词里的 `clip()` 按**字节**截断（`&text[..max]`），中文一出现就 panic。turn 的 task 直接
   死掉，而清理状态的代码在它后面 —— 于是会话永远停在 `running`：没有请求在飞、没有 CPU、
   没有日志，和「provider 不答了」一模一样。现在按字符切（`char_indices`），并有单测守着。
2. turn 现在整体包在 `catch_unwind` 里：再 panic 也会落到 `Error` 状态并打一条 error 日志，
   而不是永远 running。

排查这类问题的抓手是分段日志（INFO 级，带 elapsed）：`turn start/end`、`step start`、
`tool start/end`、`model request: sending/headers/stream ended`、`compression request done`。
看到最后一条是什么，就知道卡在哪一段。

### 非交互入口

```sh
gongga set_session --name demo                       # 命令本身就是子命令
gongga send_message --message "hi" --session demo    # 拿到回执就退出
gongga "set_session --name demo"                     # 整行形式，脚本里拼出来的也能用
```

SPEC 里没有。加它是因为：没有非交互入口就没法脚本化，也没法做端到端冒烟
（TUI 需要 TTY）。

输出**永远且只有 JSON lines**：每个服务端事件一行 `serde_json` 对象，没有给人看的文本
（`NullSink` 吞掉所有渲染）。`CmdResult{ok:false}` 会在打完那行之后以非 0 退出码结束，
错误信息同时进 stderr。所以 `jq`、脚本、以及「模型在 `bash` 里读自己的输出」是同一种
消费方式，不需要为它们各准备一种格式。

例外只有一种：一行命令在**本地**就被拒（名字不是命令、缺必填参数）时，它根本没上过线，
于是没有任何事件可打 —— 这时只有 stderr 上那句具体原因（`unknown command "exec": ...` /
`send_message is missing required argument(s): session`）和非 0 退出码。原因是从 sink
捞回来的，不是一句笼统的「没解析成命令」。

**`send_message` 是 fire-and-forget**：服务端把消息收下就回 `message accepted by X`，
CLI 立刻退出，turn 在后台自己跑完，跑完时 assistant 的消息才落进历史。
不再有「等到会话 idle 才退出」——那需要一个「等多久」的旋钮，而这个旋钮和 bash 工具的
`timeout_ms` 是重复的，两个超时各说一套比没有更糟。想看结果就再发一条命令去查，或者开 TUI。

**为什么没有 `exec`**：它曾经只多给 `--timeout` / `--json` 两个开关。timeout 和 bash 工具
的 `timeout_ms` 重复，JSON 变成唯一输出格式后也没有什么可开关的了，所以整个子命令删掉 ——
现在只有上面这一种写法。

**为什么不是 `gongga set_session` 这种真子命令**：能做，但命令集是服务端握手时下发的，
本地没有固定列表可声明，所以走 clap 的 `external_subcommand` 兜住任何名字，再交给和
TUI 输入行**同一套** `submit()` 解析 —— 否则补全、sticky 补齐、报错要维护两份。
代价：`--server` / `--home` / `--key` 必须写在命令**之前**（external 参数会被整包吞掉），
所以检测到它们出现在命令后面时直接报错，而不是悄悄连到默认地址。

非交互模式**没有等待时限**：等的是一条命令的回执，不是它启动的活。服务端不回就会被自己的
`idle_timeout` 断连，那也是一个错误出口，不需要客户端再猜一个数字。

## 6. 配置

`~/.gongga/config.toml`（`--home` / `GONGGA_HOME` 可改），首次启动自动写一份带注释的模板。
所有字段都能被 `GONGGA_*` 环境变量覆盖。

```toml
[server] bind = "127.0.0.1:9700"  db = "~/.gongga/gongga.db"  key = "<32 hex>"
[llm]    base_url = "https://api.minimax.cn/v1"
         model = "MiniMax-M3.1-Flash"
         api_key_env = "MINIMAX_CN_API_KEY"
[tools.bash] timeout_ms = 120000  max_output_chars = 8000
[agent]  max_steps = 64  compress_above_chars = 120000  keep_recent = 30
[transport] mtu = 1200  window = 64
```

`max_output_chars = 0` 关掉输出上限，`compress_above_chars = 0` 关掉压缩。

CLI 的 sticky 参数单独放 `~/.gongga/cli.toml`，和服务端配置分开 —— 它们是客户端状态，
服务端不该看见。

Git Bash 自动探测常见安装路径，找不到再退回 PATH 上的 `bash`。

## 7. 验证情况

| 项目 | 状态 |
|---|---|
| `cargo build --workspace` | 通过 |
| `cargo clippy --workspace --all-targets` | 无警告 |
| 单元 + 集成测试（101 个） | 通过 |
| 真实 UDP：200KB 分片往返 / 25% 丢包恢复 / 顺序性 | 通过 |
| 端到端冒烟（**真实 MiniMax-M3.1-Flash** + 真 Git Bash） | 通过 |
| 端到端冒烟（假 LLM + 真 Git Bash，`detached` 工具调用） | 通过 |
| key 鉴权：错 key 被拒 / 对 key 放行 | 通过 |
| 命名 bash + `del_bash` | 通过 |
| `background` 作业输出由 `check_bash` 取走（真 Git Bash） | 通过 |
| `detached` 立刻返回 + `cancel` 能杀掉（真 Git Bash） | 通过 |
| sticky 参数客户端持久化 + 自动补齐 | 通过 |
| `send_message` 缺 `--session` 被拒 / 有 sticky 才补齐 | 通过 |
| 裸文本不再被当成消息：输入行只有命令一种语法 | 通过 |
| 状态条不含「当前 session」 | 通过 |
| `cancel`：回执发出 → 停服 → 进程退出 | 通过 |
| detached 作业**跨服务端退出存活** | 通过 |
| detached 作业被下一个服务端 `adopt()` 接管并仍可杀 | 通过 |
| reasoning `summary` 从 delta 累积（真实 provider：0 → 395 字符） | 通过 |
| 回传的 `encrypted_content` 被 provider 消费（真 blob 答对 / 垃圾 blob 答不出） | 通过 |
| 工具输出上限：22KB `cat` → 2004 字符 + 落盘（真实 provider） | 通过 |
| 摘要压缩：第 51 条处压出 2318 字符摘要，会话继续（真实 provider） | 通过 |
| 模型从 bash 里调自己的 CLI（`$GONGGA_CLI cancel`，真实 provider） | 通过 |
| CLI 裸命令 / 整行引号形式两种写法，输出恒为 JSON lines | 通过 |
| `send_message` 拿到 ack 就退出（实测 0.45s），turn 在后台跑完才落库 | 通过 |
| `cancel` 后 pending 行**保留**，下个服务端 `resumed=1` 重放（真实 provider） | 通过 |
| `set_session` / `del_session` / 不存在的会话报错 | 通过 |
| TUI 交互输入、补全弹窗 | **未验证**（需要真 TTY，只能人工开一次） |
| 滚动：滚轮 / Ctrl+↑↓ / Shift+↑↓ / PgUp/PgDn，两端 clamp | 逻辑有单测，**手感未验证** |

端到端实录（真实 provider，会话 `demo`）：reasoning 流式 → tool-call 参数流式 →
`bash` 真实执行 `echo hello-from-gongga && pwd` 返回
`hello-from-gongga\n/c/resource/gongga` → 第二轮拿到最终回答。
另一次让模型用 `name:"build"` 开第二个 shell 再 `del_bash` 删掉，服务端日志确认
`shell started bash=build` 且模型随后正常收尾。

最近一轮（假 LLM + 真 Git Bash，端口 9798/9800）：`set_session` → sticky `session=main`
→ `send_message` → 模型调 `bash{"cmd":"sleep 12; echo survived > flag","detached":true}`
**没有阻塞** → 立刻 `cancel`（回执：`cancelling 1 session(s); 0 pending task(s); 64 detached
job(s) keep running -- they outlive the server, stop them with kill_bash`）→ 服务端进程退出、
端口释放；**12 秒后 flag 出现**，说明那个进程确实跨过了服务端的死亡。
同一库再起一个服务端（9801），启动时清掉已结束的行、接管仍在跑的（该次 64 行 → 31 行被接管）。

## 8. 遗留问题（需要你定）

1. **UDP 只鉴权不加密**（见 §2）：流量内容对同网段可见。要不要上 DTLS / 换 QUIC？
2. **`response{session,...}` 的语义**（见 §3）。
3. **是否需要给 agent 暴露 DB 工具**（见 §4）。
4. **bash 零审批**下要不要危险命令黑名单（见 §4）。
5. **非交互输出只有 JSON**（见 §5）：人直接敲 `gongga set_session --name x` 看到的是一行
   JSON 而不是「session is now x」。TUI 才是给人看的，命令行是给脚本和模型看的 —— 是否
   要留一个 `--text` 兜底？
6. **同一会话并发**：现在新 turn 会取消旧 turn；并行工具调用是串行的。是否符合预期？
7. `set_session` 目前带了个 SPEC 没写的 `--cwd` 参数（新建会话时指定目录）。要不要去掉？
8. **`cancel` 现在会停掉整个服务端**（按 SPEC 的「then exit」字面理解）。Ctrl+X 是另一条
   路径（`ClientMessage::Cancel{session}`），只取消单个 turn 且**不**留 pending。要不要
   反过来：让 `cancel` 只停 turn 不停服？
9. **`detached` 不再跑在常驻 shell 里**（见 §4）：它拿不到 shell 的 export / 函数，
   也不能用 `check_bash` 收输出，只能读日志文件。这是「能跨服务端存活并可被叫停」换来的
   代价，是否接受？
10. **`kill_bash` 是 SPEC 之外新增的第 5 个工具**（`bash` / `check_bash` / `list_bash` /
    `del_bash` / `kill_bash`）。没有它 detached 作业就没有任何结束方式；但要不要改成
    `del_bash{name, job}` 这种「复用已有工具」的形态？
11. 仓库还没 `git init`。
12. **模型拿不到 `cancel`**（见 §4）：工具里只有 bash 系列，`cancel` 是客户端命令。所以
    agent 想停服务端只能自己 `taskkill`（实测 `test1` 就是这么做的）。要不要给模型一个
    停自己的途径，还是就保持「模型没有这个权限」？
13. **`detached_jobs` 里那条 job 就是服务端自己**（`test1` 的 pid 31984 是 `exec` 出来的
    新 server 的 MSYS wrapper）：`kill_bash` 一下就能自杀，而且重启 adopt 时有 pid 复用
    误伤的风险。要不要在 adopt / kill 时跳过「命令看起来是自己」的作业？
14. **压缩后 reasoning 链可能断**（见 §4 闸二）：保留区间里的加密块引用的前文已经变成
    摘要，provider 是否还消费它，目前没有直接证据。
15. **一次压缩 ~32s**：实测压缩请求 32s（2000 token 上限、非流式）。它不是每步都发生 ——
    `context()` 只统计 `summarised_upto` 之后的部分，而 `choose_cut` 要求 live 多于
    `keep_recent`(30) 条才切，压完 live 就只剩那 30 条，闸门直接关上（实测压完后再发消息：
    `step start` 前没有任何 compression，turn 1.1s 结束）。
    曾经浪费一次：启动 resume 的 turn 和新发的 turn 各自判了一次超预算，同一份上下文被压
    两遍、64 秒。**已解决** —— 同会话的 turn 现在排队（见 §4「冲突的操作排队」），第二个
    turn 开始时会看到第一个压好的摘要。
16. **压缩期间不吃 cancel**：turn A 被新 turn 取消后，它那次 32s 的压缩照样跑完才退出
    （日志里 `turn end outcome=Cancelled` 出现在压缩之后）。因为请求期间没有 select 取消信号。
    要不要让压缩也能被打断？
17. **排队的 turn 没法单独取消**：Ctrl+X（`ClientMessage::Cancel`）只有 TUI 有入口，CLI 的
    非交互模式发不出来，所以脚本和模型都只能看着队列里那个 turn 等着。要不要给一个
    `cancel_turn --session x` 命令？（`cancel` 是停服，不是这个语义。）
18. **`del_session` 会一直等到 turn 结束**：排队是对的，但一个跑了 20 分钟的 turn 会让
    `del_session`（以及排在它后面的所有命令）挂同样久，且 CLI 已没有超时上限。要不要给等待
    加一个上限，或者让等待中的 `del_session` 先取消那个 turn 再删？
