# Gongga Harness — 设计说明

依据 `SPEC.md` 实现，并记录了实现过程中做出的判断。其中标注 **【待确认】** 的是偏离或有歧义的地方，需要你拍板。

## 1. 结构

```text
Cargo.toml                  workspace，resolver 3，edition 2024
crates/protocol/  gongga-protocol   别名 protocol —— 帧、可靠 UDP 通道、消息词汇
crates/server/    gongga-server     别名 server —— **库**：没有二进制，由 cli 内嵌运行
crates/cli/       gongga-cli        别名 cli —— 命令层（库，外加 `gongga` 命令行程序），
                                   没有服务端时自己起一个（`cli::server`）
crates/tui/       gongga-tui        别名 tui —— 终端前端 + `gongga-tui` 二进制
crates/bridge/    gongga-bridge     别名 bridge —— IM 桥接器，见 §9
```

命名沿用你 `whiting` 项目的约定：目录不带前缀，`[workspace.dependencies]` 里用
`package = "gongga-xxx"` 做别名。

**依赖方向是一条链：`tui → cli → server`，反向不存在。** 服务端是库，命令层是内嵌它的那个
二进制，窗口内嵌命令层。`cli` 依赖 `server` 是因为 `serve` 这条客户端命令要就地变成服务端；
`server` 不知道 `cli` 存在（它连 CLI 二进制在哪都不搜了，只认 `<home>/bin` 这个目录）。

第二条是反的（`cli` 依赖 `tui`）的那些日子：交互模式
当初写在 `gongga` 二进制里，而那个二进制归 `cli`。这看上去只是 Cargo.toml 里一行，实则把
ratatui 与 crossterm 拖进了**每一个**只想发一条命令的调用：脚本、测试、桥接器、模型手里的
`$GONGGA_CLI`，全都要为一个它们不用的终端付出依赖与编译代价。命令层一旦认识终端，「同一条命令
也能从没有键盘的地方发出」这个性质就守不住了。

现在的分工：

| crate | 是 | 不是 |
|---|---|---|
| `cli` | 命令目录、sticky、参数解析/渲染、`Client` 状态机、链路 `Link`、`LineKind`、`gongga` 命令行、`serve` 起的服务端 | 任何终端 |
| `tui` | 终端渲染、输入行、补全弹窗、交互循环、`gongga-tui` 二进制 | 命令是什么、协议长什么样 |

两个程序，各归一半。`gongga` 是命令层自己的 front-end：argv 进来，这条命令的事件出去，视野里
没有终端；不带命令时它打印服务端在问候语里发布的那份目录。它也是**唯一能当服务端的程序**：
`gongga serve --port <p>` 就地跑内嵌的 server 库（§5「谁起服务端」）。窗口是 `gongga-tui`，它是**组装点**，
也是唯一同时需要两半的程序 —— 按 `cli` 的定义解析命令行、把「没有命令」交给交互循环、把有命令
的交给 `cli::run`。两者都不认识任何命令名，所以「加一条命令要改前端」这件事不成立（SPEC 的
「tui: transparent cli wrapper, do not know cmd」就是这条）。把自己叫什么也交给调用者：
`cli::parse_argv(name, about)`，于是 `gongga --help` 与 `gongga-tui --help` 说的都是被运行的
那个东西。

**为什么 `tui` 连协议也不认识**：交互循环要收包发包，但它拿到的是 `Link::recv()` 给的字节和
`Client::handle_bytes()` 的结果；问候语是一个不透明的 `Greeting`，应用它的是 `cli`。所以 `tui`
的依赖里没有 `protocol` ——「线长什么样」仍然只有命令层知道。代价是 `cli` 多了三个薄封装
（`Link::send/recv/goodbye`、`Client::handle_bytes`、`Greeting::apply`）。

**方向有测试钉着**：`cli` 里断言「依赖里不得出现 tui/ratatui/crossterm，且它的程序就是命令行
那一个 `gongga`」，`tui` 里断言「必须依赖 cli」。两者都读 `include_str!("../Cargo.toml")`，
谁把箭头调回去，测试会先红。

**`tui` 的边界**：SPEC 说「tui: transparent cli wrapper, do not know cmd」。所以 tui 只认识
「行」「候选词」「按键」，不认识命令、不认识协议。词汇本身由命令层定义、tui 拿来用
（`pub use cli::{Candidate, Completer, Completion, LineKind, SharedCompleter}`，所以
`crate::LineKind` 在 `render.rs` / `log.rs` 里照旧可读）：

```rust
// cli（命令层）定义：
pub trait Completer { fn complete(&self, line: &str, cursor: usize) -> Vec<Candidate>; }
pub enum LineKind { System, User, Assistant, Reasoning, ToolCall, ToolOutput, ToolError, Result, Error }
// tui（前端）自己定义：
pub enum UiEvent { Submit(String), Quit }
```

补全候选由 `cli` 注入（它持有服务端下发的命令目录），tui 只负责弹窗和插入。
验证方式：给 tui 换一个 completer 它照样工作，说明耦合真的断开了。

**tui 不认识命令，所以也不该替命令做决定。** 早先 `UiEvent` 里有个 `Cancel`（Ctrl+X），
看上去只是「多一个键」，实际上是给「停掉 turn」开了一条绕过命令层的小路：只有 TUI 有入口，
脚本和模型都没有。现在打断/停服都只是普通命令（`interrupt_session` / `cancel`），
`UiEvent` 只剩下「提交一行」和「离开」（Alt+Q）。

## 2. 传输层：可靠 UDP

按你的选择走「跨机 + 自研轻量可靠层」。没有用 QUIC（强制 TLS，跨机自签证书很麻烦），
也没有用 KCP 绑定（API 是回调式的、非 async）。自研约 500 行。

### 帧头（26 字节，定长）

```text
 0.. 2  magic    u16  0x4747
 2.. 3  version  u8   1
 3.. 4  flags    u8   SYN|ACK|FIN|RST|DATA
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
- **入站投递不阻塞链路**：交给调用方的消息走无界队列，I/O task 不会因为消费端慢而停下 ——
  同一个 task 还负责 ACK 和重传，停在投递上会让**两端都活着、链路却像挂了**。背压改用接收
  窗口表达：通告窗口 = `cfg.window` 减去调用方还没取走的消息数（最少 1），消费端一慢，对端
  就少发。消息不丢、顺序不变。
- **读 socket 的 task 与连接同生共死**：`Channel` / 两个半边的最后一个被 drop 时 abort reader。
  它平时阻塞在 `recv_from` 上，对端不再发包就永远醒不过来 —— 每次重连都会留下一个 task 和
  一个 `Arc<UdpSocket>`。

`tests/loopback.rs` 用真实 UDP socket 验证：200KB 消息分片往返、25% 丢包下恢复、
多条消息保持顺序、消费端暂停时链路不停、连接 drop 后 reader task 释放 socket。
丢包恢复从最初的 25s 优化到 <0.4s。

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
ClientMessage: Hello{client,key} | Command{id,name,args} | Bye
ServerMessage: Hello{protocol,server,commands,sessions} | Denied{reason} | Event(event)   # 相邻标签 {t,v}
Event:         Cmd | CmdResult | Response | ToolAnswer | Sessions | Status | Log
```

`ServerMessage` 用**相邻标签**（`tag="t", content="v"`）而不是内部标签：
`Event` 自己也带 `t` 判别符，内部标签嵌套会序列化出两个 `t`，客户端直接反序列化失败。
这个坑在第一次端到端冒烟时踩到了。

**sticky 参数不在协议里**。SPEC 把它放在 UI 一节（`sticky_cmd_arguments[{name,value}]`），
所以它是纯客户端概念：CLI 保存在自己的 `~/.gongga/cli.toml` 里，发送前补齐，服务端完全不知道。
早先版本让服务端存 `cmd_args` 表、在 dispatch 前补齐 —— 已按 SPEC 删掉。

`response{session, reasoning, toolcall, output}` 的实现：

```rust
ResponseEvent { session: String, turn: u64, part: ResponsePart, text: String, done: bool }
enum ResponsePart { Reasoning, ToolCall, Output, Activity }
```

即 **session 是归属字段，reasoning/toolcall/output 是三条可交错的子流**，每个 delta 带
`part` 和结束标志。**【待确认】** 这是我对你 SPEC 里那行记法的读法，另一种可能是
`session` 也是一条子流。

`turn` 从 1 递增，客户端可据此丢弃 cancel 之后到达的陈旧 delta。

`get_messages` 不再把 transcript 塞进 `cmd_result.data`，而是把每一行**按它当初被发布成什么
事件就重发什么事件**：text/reasoning/tool_call/builtin_call 走 `response`（part 一一对应），
tool_result 走 `tool_answer`（按 call_id 归属，正是直播那一块封存的位置），notice 与 system
是 harness 说给读者听的话，走 `log` —— `agent.rs` 写 notice 那行时发的就是 `log`。事件词汇表
因此还是 SPEC 那七个，没有第八种要学的形状。它们和 `cmd`/`cmd_result` 一样是**定向投递**的
（只发给发起读取的那条连接，`net.rs` 的 `run_commands` 在结果之前把它写完），所以别的客户端
不会看见你的读取；而前端画读回来的行用的还是画直播的那条路 —— 颜色、块、限高、滚动全都一致。
附带解掉一个老限制：一条回执太大就整条发不出去（`net.rs:361-374` 会回 `ok:false`），
拆成一行一条之后，读一大段不再被体积打死。

代价是行上的两个标识不在线上了。`turn` 用 `REPLAY_TURN = 0`（直播的 turn 从 1 起）：它既让
一条重发的行自成一个块、不会接进正在写的那条答案里，也是前端区分「读回来的」和「直播的」唯一
记号（bridge 靠它决定要不要缓冲）。`seq` 只在回执里报（`last_seq` 与那句 `seq a..b`）——
推进游标要的是它，「读到第几行了」不再逐行标在屏幕上。

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

pid 是**通过 `create_new` 返回的那个句柄**写进去的（`write_all` + `sync_all`），不是事后再
`fs::write` 一次：文件在 `create_new` 返回的瞬间就对别人可见，先建后写会给第二个服务端一个
空文件的窗口，而空文件会被当成「陈旧」接管 —— 两个服务端于是共用一份 home。同理：

- 读不出 pid 的锁**不算陈旧**，等 250ms 再拒绝启动（「另一个服务端正在启动」），不接管；
- 陈旧锁用 `rename` 挪开再由 `create_new` 决定归属：两个服务端同时判定陈旧时，`rename`
  只有一个能成功，接下来的 `create_new` 也只有一个能赢；
- `Drop` 只删写着自己 pid 的那份锁。

### 冲突的操作排队，不抢

一个会话上的操作共享同一份 transcript，所以会互相踩的都得**排队**，而不是靠运气。排队的
是 `SessionHandle` 上的一把 FIFO 门（`tokio::sync::Mutex`，先到先得）：

| 操作 | 为什么和别人冲突 |
|---|---|
| turn | 两个 turn 会各自量一遍上下文、各自判定超预算、把同一份内容压两遍 |
| `del_session` | 会把 transcript 从正在写它的 turn 脚下抽走（它**先 interrupt 再删**，见下） |
| 同一 shell 的 `bash` | 一条命令的 `cd` 会改掉下一条的工作目录（本来就有 per-shell 锁） |
| detached 注册表 | 已有全局锁 |
| `Store` | 单条 SQL 有连接锁，但跨调用的读-改-写靠上面这些排队才成立 |

拿门必须在 `new_turn()` **之前**：`new_turn` 的语义是「取消正在跑的那个」，一个还没被放行的
turn 调用它，就会杀掉自己本该排在后面的那个 turn。所以现在「发消息」的意思从"打断"变成
了"排队"—— 想打断是 `interrupt_session` 的事。

排队中的 turn 有两种「不用跑了」的情况，都会在放行时立刻放弃，不留任何痕迹：

- **等待期间被 `interrupt_session`**：`cancel` 递增一个计数，turn 拿门前后各读一次，变了就走人。否则会
  出现"我明明取消了，它怎么又开始跑"。
- **等的是一次 `del_session`**：会话已被删除，写下去等于把用户要求删掉的行复活。

**排队不等于干等**：`del_session` 是唯一会在删除前**先 `interrupt`** 的命令。删掉一个会话
就是不要它正在做的事，乖乖排在它后面意味着「等这个 turn 自己跑完」—— 它跑 20 分钟，删除
就要 20 分钟。所以删除先打断、再拿门（此时只需等 turn 松手，是毫秒级）、再删。实测：会话
正在跑 `sleep 45` 时删除，**0.49 秒**返回，bash 在 11.6 秒处被中断（`tool end ok=false`），
回执写明 `(interrupted the running turn); dropped 1 pending task(s)`。

其余的等待只打一条日志（`turn waited for the session to be free`），否则一个排了三分钟的
turn 看起来和服务端卡死没有区别。需要打断而不是排队时，用 `interrupt_session --session <n>`：
它既停正在跑的 turn，也通过同一个取消计数让排在后面的 turn 放弃，所以脚本和模型都能用它
（以前只有 TUI 的 Ctrl+X 能打断）。

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
来自客户端的 `sticky_cmd_argument`，它在命令上线前就被填好。服务端不保存任何
「你上次用的是哪个会话」这类隐藏状态。

`pending_tasks` 是未完成的活：一个 turn **开始前**写行、**结束后**删行，所以崩溃留下的痕迹
和干净退出相反 —— 表里有啥，下次启动就欠啥。

两种 cancel 的区别只在这一行上：

| 谁 | 语义 | pending 行 |
|---|---|---|
| `interrupt_session --session <n>` | 「别干了，丢掉」 | 删 |
| 服务端命令 `cancel` | 「停下来，剩下的下次接着干」 | **留**（`cancel_and_preserve`） |

第二行以前是坏的：`cancel` 只置了取消标志，turn 自己收尾时照清不误，于是回执里
「N pending task(s) will be picked up」是句空话 —— 只有被 `taskkill` 硬杀才真的留得下。
现在 `cancel` 走 `cancel_and_preserve()`，turn 收尾前先看这个标志。

**`cancel` 之后进程退不掉（已修）**：日志已经打出 `server stopped`、UDP 端口已经放掉、home
的锁已经删掉，进程却一直留在任务管理器里，只有在**用过 `bash`** 的服务端上出现。根因不在关闭
流程 —— 在它之后的 `#[tokio::main]`：那个宏展开出的 `Runtime` 在 `Drop` 时无限期等待所有
blocking 任务结束，而 tokio 在 Windows 上正是用 blocking 任务实现 `Child::wait`；持久 shell
是一个**设计上就不退出**的子进程，于是这个等待永远不返回。空载的实例 100ms 就退，跑过一条
命令的实例永远不退 —— 这解释了为什么以前只见过「`taskkill` 才管用」。

现在 `main` 自己建 runtime，`run()` 返回后先 `close_shells()`（把每个会话的 shell 连同它启动
的整棵进程树结束掉），再 `drop(runtime)`，全程没有 deadline。`main: dropping runtime` /
`main: runtime dropped` 两行之间的时间就是真正的排空时间。

为什么必须先结束 shell：tokio 在 Windows 上把子进程的管道读放在 blocking 线程上，一次读要等
**所有写端关闭**才返回。`sleep 240` 这样的命令是 shell 的子进程，继承了管道的写端 —— 只杀
shell 不管它，读线程就永远等下去。而 `taskkill /T` 收不干净：它从根往下扫，扫描和根死亡之间
新 fork 出来的进程会变成孤儿，照样握着管道。所以 Windows 上每个 shell 建一个 **Job Object**
（`KILL_ON_JOB_CLOSE`），启动命令前先入 job，关闭句柄时整棵树一起结束；unix 用 `process_group(0)`
+ `kill -TERM -pid`，本来就是树。

`cancel` 的等待也去掉了上限：`cancel` 把同一个 token 交给每个运行中的 turn，turn 在
model 流、工具调用、重试退避、压缩上都 select 它，在 step 边界退出且结果已提交，所以
`active` 一定能归零。`active` 的加减改成 `ActiveGuard`（`Drop` 里减），turn panic 也会释放。
真出现不退的 turn，每 10 秒打一条 warn 说明还剩几个 —— 是观测，不是砍断。实测：

| 场景 | 修复前 | 修复后 |
|---|---|---|
| 空载 | 100ms | 260ms（含 250ms 回执让路） |
| 跑过命令、shell 还开着（`active=0`） | >2 分钟仍在 | <1s |
| turn 正卡在 `sleep 240`（`active=1`） | >2 分钟仍在 | 0.5s |

`detached_jobs` 是**跨服务端存活的进程**：`bash --detached` 启动一个就写一行
（session / shell / job / pid / cmd / log），停掉才删。它是 detached 语义的地基：
进程不归服务端所有，所以「谁还活着」这件事只能靠数据库交接给下一次启动。

**【待确认】** 是否需要进一步给 agent 暴露一个 DB 查询/改写工具？目前没有。

连接放在 `Arc<Mutex<Connection>>` 里同步调用。这里的语句都是本地亚毫秒写；若哪天不成立，
修法是 `spawn_blocking`，不是改架构。

### Agent loop

一个 *step* = 一次模型请求 + 它调用的工具；一个 *turn* = 若干 step，直到模型不再要工具、
或被取消。**没有步数上限**：长程任务的步数无法事先估出来，任何数字要么太小（任务中途
停下等人类敲「继续」——那不叫自主，叫陪跑），要么大到不构成限制。保证一个回合真正
有限的是工作本身结束，保证它不空转的是请求失败被如实上报，而不是静默截断。
每一步之后 transcript 已经落盘，所以中途崩掉留下的是可重放的会话而不是空洞。

- 取消：per-session `CancellationToken`，新 turn 会取消旧的。
- 工具顺序执行（并行对持久 shell 没意义）。
- 失败/取消通过 `Event::Status{status}` 和 `Event::Log` 上报，不向调用方返回错误。

#### 外部事件在 step 边界注入

会话有一个 per-session inbox（`pending_tasks` 表，持久）。两类东西进去：用户中途发来的
`send_message`、后台作业完成的通知。

**唯一的消费点是 agent loop 的 step 边界**——每个 step 的工具跑完之后、下一个请求组装之前，
`deliver_inbox` 把 inbox 排空，按 `text/role:user` 写进 transcript，下一个 step 的请求自然
读到。不能在消息到达时立刻写：一条用户消息如果插在 `function_call` 和它的
`function_call_output` 之间，provider 会整单拒绝。step 边界是唯一合法的位置。

- `send_message` 撞上正在跑的 turn 时**不再排第二个 turn**（那要等当前 turn 结束才被读到，
  正是最不该等的时刻），而是写成 `kind=inject` 的行，由当前 turn 下一个 step 读掉。
- 只有会话 idle 时才起新 turn：`nudge_inboxes` 每 2 秒看一眼有没有 `inject` 行落在没人跑的
  会话上（turn 刚好先结束、或后台作业在 idle 时完成），有就起一个 resume turn。
- `take_pending_tasks` 在一个事务里「取出即删除」，所以消息要么还在 inbox、要么已在
  transcript，不存在重复投递或丢失。turn 自己那行用 `except` 排除——它的消息在 turn 开始
  时就写过了，再投一次就是用户的每句话出现两遍。
- `kind` 区分契约：`turn` = 「消息已在 transcript，这行只证明还欠一个 turn」；`inject` =
  「消息还没落库，谁消费谁负责写」。重启时 `resume_pending` 按这个区别对待。
- **`busy()` 只读一次**，同一个快照决定 `kind` 和「要不要起 turn」。读两次的窗口很窄但后果
  很难查：第一次说忙（写 `inject`），第二次说闲（起 turn 并把它写进 transcript），行却没删，
  崩溃后 `resume_pending` 再写一遍 —— 用户的每句话会重放两次。

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

**闸二：摘要式压缩（默认填到模型窗口的 70% 触发）**

每个 step **之前**检查一次：若上一次请求实际带进去的 input tokens 超过阈值，就把较早的部分
交给模型压成一条 handover note。

- **阈值是 token，不是字符，也不是算出来的**。provider 每条响应都返回
  `usage.input_tokens` —— 那是它自己按自己的 tokenizer 数出来的、即将对它收费的数字，覆盖了
  instructions、工具 schema、摘要和整段 transcript。把它存进 `sessions.context_tokens`，下一步
  拿它和阈值比。**没有任何换算**：本地算字符/token 比率对 15 家 provider 就是猜（tiktoken 只对
  OpenAI 准），而真值一直免费随响应回来。
  - 存盘而不是只放内存，是为了让重启后的第一个请求也有尺寸可比；那一刻还没有新的测量值。
  - 压缩成功后把这个数清零：它描述的是刚被折掉的旧请求，留着会让下一步拿过期数字再压一次。
  - 兼容层不返回 `usage` 时，这个数一直是 0，压缩不会触发 —— 这是唯一诚实的失败方式，
    总比用一个假比率把上下文撑爆、然后被 provider 400 强。
- **阈值从哪来**：模型有 `context`（`~/.agents/llm.toml`）时就是它的 70%
  （`context_limit` 可改百分比）；没有 `context` 时退回 `agent.compress_above_tokens`
  （默认 40000）。0 一律表示关闭，包括全局配置和百分比两条路。

- **非破坏性**：`sessions.handover` 只记「上一轮留给自己的话」，一行消息都不删 ——
  transcript 是记录不是缓存，删了就没了，且 UI 仍能回放全部历史。请求 = 一条 user 消息
  （笔记前缀）+ 表里**全部**消息：没有「折到第几条」这种边界。
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

默认 provider 是 **minimax-cn / MiniMax-M3.1-Flash**：

| 项 | 值 |
|---|---|
| `base_url` | `https://api.minimax.cn/v1` |
| `model` | `MiniMax-M3.1-Flash` |
| `api_key_env` | `MINIMAX_CN_API_KEY` |
| key 来源 | 环境变量 → **`~/.dsh/.credentials.yaml` 的 `refs:` 块** |

已实测：`https://api.minimax.cn/v1/responses` 接受标准 Responses API 请求，无需代理。
（该 host 的 `/anthropic/v1` 是 Anthropic 兼容端点；我们按 SPEC 走 Responses，所以用 `/v1`。）

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
  **没拿到结果的 tool_call 会被补一条结果再发**：turn 在「调了工具」和「工具回话」之间被
  打断（`cancel`/`interrupt_session` 都可能），transcript 里就留着一条只有
  `function_call`、没有 `function_call_output` 的记录。DeepSeek 对它的回答是整条请求 400
  （`No tool output found for tool call ...`），于是这个会话**之后每一轮都同样失败** ——
  实测踩到了。现在 `history_since` 给这种调用补一条
  「本轮被中断，结果未知，请重新确认」，既让请求合法，也没替模型假设命令跑没跑。

#### 名单：`~/.agents/llm.toml`（自动导入）

这台机器有一份机器级的 LLM 名单：15 个 provider，各自带
`[[provider.endpoint]]`（`api` + `base_url`）、`[[provider.model]]`（`id` / `context` /
可选 `endpoints`）、以及 `key` 字面量或 `key_env`。服务端启动时直接读它，
所以「哪个 provider、什么 base_url、key 在哪、上下文多大」不再需要在 config.toml 里重抄一遍
（那份文件里那一大堆价格、额度、节假日日历一概忽略 —— 解析只取发请求要用的字段）。

选择顺序（越具体越优先）：
1. `GONGGA_PROVIDER` + `GONGGA_MODEL`（或 config 的 `[llm] provider`/`model`）→ 精确命中那一条；
2. 只给了 model → 在名单里找带这个 model 的 provider；
3. 什么也没给 → 名单里**第一个**同时满足「有 OpenAI 兼容端点」「模型能用 Responses」的条目，
   文件顺序就是偏好顺序。

`base_url` / `model` / `api_key_env` 仍是「环境变量 > config.toml > 名单 > 内置默认」，
所以老配置不会被新逻辑悄悄改掉；key 的链条是
`GONGGA_API_KEY` → 该 provider 的 `key_env` → 名单里的 `key` 字面量。

`--llm <provider/model>`（也接受裸 model id）就是在这份名单里点名：
在 `set_session` 时解析，**错了当场报错并列出可选项**，而不是留到下一轮才失败。
`--context_limit 30%` 就是 `context` 的 30%，直接是一个 token 数，没有任何换算。
模型没有 `context` 时这个百分比没有大小，所以命令会直接拒绝，而不是悄悄退回旧预算。

**会话有自己的模型，不跟服务端的默认走**：reasoning 项里那份加密块只有产出它的 provider
能解开（见上一节），把会话悄悄换到另一个模型等于把它的上下文作废。所以
`set_session` 创建时会记下当时的条目，老会话（`llm` 列为空）用 `model` 列去名单里反查。
实测：名单导入后默认变成了 `deepseek-official/deepseek-flash`，而 `test1` 这条老会话仍然
按 `minimax-cn/MiniMax-M3.1-Flash` 跑。

名单缺了或坏了都不致命：记下原因、退回内置默认、启动日志里写明
（`llm roster imported roster=... providers=15`）。为了让名单不是个更糟的依赖，
这里宁可少干也不拒启动。


### 工具（SPEC: bash / check_bash / list_bash / del_bash，外加 del_bash）

Shell **按 `session_name/bash_name` 建键**，所以一个会话持有的是一组常驻 shell，不是单个：

- `default` 在会话物化时就存在。
- `bash` 带上别的 `name` 时**按需 spawn**，起始目录是会话目录。
- `del_bash{name}` 丢弃一个 shell；下次再点名会重新 spawn，因此 del 之后 bash 依然可用
  （只是状态清零）。这正好对应「shell 被搞脏了就换一个」。
- 每个 shell 是常驻 Git Bash（`--norc --noprofile`，stdin 管道驱动），cwd / export /
  shell 函数跨调用保持。
- 命令结束后向 **stdout 和 stderr 各打一个带退出码的哨兵**，两个流可以独立收尾又都能拿到状态码。
  哨兵是每 shell 随机的，避免输出里恰好出现相同文本导致截断。
- 超时（默认 120s）**只结束等待，不结束命令**：shell 留着继续跑，会话里同名 shell 换成新
  spawn 的一个（cwd 回到会话目录），旧 shell 的后台读取任务在命令结束时把剩余输出和退出码
  作为一条 inbox 消息投回会话（`kind=inject`），空闲会话由 nudge 唤醒。detached 作业不属于
  这个 shell，不受影响。
  这样超时不再是「活儿最多干多久」的预算 —— 长构建、长测试只是换了个地方继续跑，模型拿到
  新 shell 立刻可以干别的。
- 每个 shell 启动时注入 `GONGGA_SERVER` / `GONGGA_HOME` / `GONGGA_CLI`(存在时) /
  `GONGGA_SESSION` 和一条 POSIX 形态的 `PATH`（重建时同样注入）。这是**模型驱动自己的通道**：`bash` 跑在服务端的
  子进程里，不告诉它这些，它就既不知道服务端的地址、也不知道自己属于哪个会话，想取消或重启只剩从
  外面 `taskkill` 这一条路。`GONGGA_HOME` 与 `GONGGA_SERVER` 正好是 CLI 自己读的两个 fallback，
  `PATH` 首位是服务端刚写进 `<home>/bin` 的那个 `gongga`，所以按名字调用就行：
  `gongga cancel`、`gongga send_message --message hi --session $GONGGA_SESSION` 都不需要额外
  参数。`$GONGGA_CLI` 就是 `<home>/bin/gongga` 这个 shim 的绝对路径，留给脚本和 launcher ——
  它们不该依赖那条 `PATH`。服务端不再自己去找 CLI 二进制：写 shim 是 CLI 每次运行顺手做的事，
  服务端只负责把这个目录排到 `PATH` 首位，两边只需要对「home」有一个共识。
  脚本的 env 也只有那几个 `GONGGA_*` 变量：POSIX 化的 `PATH` 是给 shell 读的，解释器要的是原生那一条。

`bash{command, name, background, detached}` 的三种跑法：

| 模式 | 是否等待 | 输出去哪 | 谁结束它 |
|---|---|---|---|
| 默认（前台） | 等，流式回传 | 直接作为工具结果 | turn 取消即中断 |
| `background` | 不等 | tail 到 shell 的**未读缓冲区**，由 `check_bash` 取走 | `del_bash` 随 shell 一起消失 |
| `detached` | 不等 | 写进自己的日志文件，**不回收** | 只有 `del_bash`；`del_bash` / `cancel` / 服务端退出都不碰它 |

- `background` 的实现：shell 把作业重定向到一个 scratch 文件（路径按 MSYS 写法转换，
  `C:\Temp\x` → `/c/Temp/x`），服务端任务每 150ms 读一次增量，追加进未读缓冲，
  看到结束哨兵就收尾删文件。`check_bash` 取走即清空，同一段输出不会返回两次。
### 一个参数名的代价：`cmd` → `command`

先说结论：**`bash` 的第一个参数现在叫 `command`，不叫 `cmd`。**

起因是一次"harness 有严重 bug"的报告。会话库（`~/.gongga/gongga.db`，1305 次 `bash` 调用）
里的真实数字：

| 观察 | 数据 |
|---|---|
| `{"command": …}` | 582 次，**582 次全部被拒** |
| `{"cmd": …}` | 723 次，66 次失败（真错误） |
| 参数平均长度 | 336 vs 357 字节 —— **与长短完全无关** |
| 错键占比 | 前 100 次 32% → 后 100 次 **68%** |

所以那轮"长命令被丢弃、短命令能落地"的自述是误判：真正相关的只有键名，长度是零相关的。
另一个容易误判的点：**拒绝是有答复的**，582 条 tool result 全都带着提示，`store.context()`
也把 `function_call_output` 回放给了模型（`store.rs` 里还专门给没等到结果的调用补占位符）。
也就是说模型是**看着**那句「用 `cmd` 不要用 `command`」硬生生又错了 582 次，而且比率
随着上下文里自己的错例变多而上升 —— 提示不但没教会它，还给它更多可以模仿的例子。

于是做了两件事，都不依赖"模型下次会听话"：

1. **参数名改成模型本来就想写的那个**。名字是给人用的，模型绝大多数情况下喊 `command`
   是对的，该让 schema 弯一下。
2. **拒绝信息由 schema 自动生成**（`argument_error`，挂在 `ToolRegistry::run` 上，所有工具共用）。
   它从 `Tool::parameters()` 里读出属性、类型、required，输出：

   ```
   `bash` did not run: `command` is required.
     unexpected: `cmd` (did you mean `command`?)
     `bash` takes: background: boolean (optional), command: string (required), detached: boolean (optional), name: string (optional), task: string (optional), timeout_ms: integer (optional)
   ```

   "did you mean" 不是同义词表，是一条规则：两个名字是否一个"按序拼在"另一个里
   （`cmd` ⊂ `command`，`command` ⊂ `cmdline`）。改名不用维护任何表，无关的键也不会被瞎猜。
   只有**缺 required** 才拒；多出来的键照旧忽略 —— 本来能跑通的调用因为一个废键被拒，
   正好是在制造这条信息本来要消灭的那种重试。

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

1. `bash{command:"gongga serve --port <new>", detached:true}` 拉起替身；
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
`del_bash` 就成了服务端自杀按钮。

代价说清楚：接管靠的是 pid 是否存活，没法证明它还是原来那个进程，理论上存在 pid 复用
窗口。这个窗口很窄，而且后果被 `del_bash`（必须显式调用）框住了。

detached 作业归**会话**所有、不归 shell：它们活在一个 `DetachedRegistry` 里
（session → jobs），`list_bash` 会把它们单独列出来（含上一任留下的），
`del_bash{name, job?}` 是唯一结束它们的方式（不给 `job` 就是停掉该 shell 的全部）。
`del_bash` 不杀它们，但会**明确告诉模型还有几个在跑**，免得模型以为丢 shell 就等于停了。
`del_session` 则相反：删除会话意味着「它起的活也不要了」，会一并停掉。

按你的选择**不做人工审批**。**【待确认】** 完全没有护栏，是否需要至少一份危险命令黑名单
（如 `rm -rf /`、写系统目录）？

### 一条二进制输出怎么让 shell 变哑

一个 shell 是**常驻进程**：命令写进它的 stdin，输出从它的 stdout 读，直到读出哨兵行
（`__GONGGA_<uuid>__<exit>`）算是这一次调用结束。哨兵就是「一条命令的输出在这里结束」这个
唯一可依赖的边界。读取用 `read_line(&mut String)`，于是有了这次的事故：

```
bash{command: "cat $(which gongga) | head -20"}       -> [read error: invalid utf-8 ... index 2]
bash{command: "$GONGGA_CLI --help"}                   -> [read error: invalid utf-8 ... index 1]
bash{command: "cat config.toml"}                      -> [read error: invalid utf-8 ... index 2]
bash{command: "echo hi"}                              -> [read error: invalid utf-8 ... index 0]
```

`cat` 一个可执行文件，把非 UTF-8 字节送进了这个 shell 的管道。`read_line` 遇到非法字节返回
错误 —— 而**这一行已经被从管道里读走了**，剩下的（包括哨兵）还在管道里。`drain` 于是 `break`，
哨兵从来没被消费。后果是这条 shell 从此**错位一格**：

- 下一次调用读到的仍是上次的残留，先撞上同一个非法字节，立刻返回同一句 `[read error]`，
  于是 `echo hi` 也成了「读错误」；
- 残留被一点点吃掉之后，读到的就是**上一次调用的完整输出**，并且**报告成功**。实测中
  `echo still-alive` 返回的是上一条 `pwd` 的输出。这比报错更坏：模型会以为命令跑过、看见了结果。

整个过程 `status` 都是 `ok`，所以从记录上看不出来 —— 只有真的去读那五次调用的 `output`
才能发现一个 shell 已经哑了，而模型当时的应对是 `del_bash{name:"cli"}` 换一个。

两处都改了（`crates/server/src/tools/bash.rs`）：

1. **按字节读**（`read_until(b'\n')` + `from_utf8_lossy`）。非法字节变成 `U+FFFD`，
   行还在、哨兵还在、管道停在命令边界上。命令想打印什么都可以，shell 不再因此失聪。
2. **丢了哨兵就不许再用**。`Drained::settled` 为假（EOF、读错误）时给 shell 打上
   `out_of_sync`，`usable()` 变假；`bash` 在返回结果的同时把它换掉（`ShellSet::replace`），
   并在 tool result 里明说「shell 已被替换，之前的 `cd` 和 export 没了」。宁可让模型知道
   环境变了，也不能让它对着一条会答错话的 shell 继续下命令。

单测 `binary_output_does_not_deafen_the_shell` 就是这次事故的原样复现（先 `printf` 出 `\377\376`
再 `echo`），`a_shell_that_ended_is_not_reusable` 管住 EOF 那一半。

3. **取消即中断**（`interrupt_session` / `cancel`）。此前取消只是丢掉等待的 future：命令还在
   那条 shell 里跑，`out_of_sync` 永远走不到置位，于是下一条 `bash` 读到上一条的输出、还报
   `ok` —— 和上面「错位一格」是同一个故障，只是入口换成了官方推荐的停止方式。现在取消分支
   会把前台命令连同进程组一起杀掉（`BashShell::kill`，同时置 `out_of_sync`），换一条新 shell，
   并在 tool result 里明说「已取消、退出码未知」。写失败（`Err`）分支也一样消费 `healthy`。

### `/` 只在输入行里存在

`/cmd` 是**输入行**的语法，不是命令层的。它本来被写在 `Client::submit(&str)` 里，而一次性
路径（`gongga <cmd> ...`）为了复用它，先把 argv 拼成一行字符串再交给 `submit` —— 于是命令
行也必须带 `/`，不带 `/` 的一律被当成说话：

```
$GONGGA_CLI cancel                        -> send_message --message "cancel"   （发了一条聊天）
$GONGGA_CLI /cancel                       -> Git Bash 把它改成 C:/.../Git/cancel
$GONGGA_CLI MSYS_NO_PATHCONV=1 /cancel    -> 这才是 cancel
```

这是 SPEC 里「`cli: cmd fully documented and diagnosed`」的反面：系统提示明明写着
`$GONGGA_CLI <command>`，照做却得到沉默的错误行为。而一次 `get_messages` 要三次调用才摸对。

根子在**层次**：argv 已经是 token 了，谁启动这个进程就是谁写下了命令名，没有任何歧义需要
再解一遍。歧义只存在于**输入框**里 —— 那里一行字既可能是命令也可能是要说的话，才需要一个
`/` 来区分。所以现在的分工是：

- `cli`：`Client::command(tokens)` 和 `Client::message(text)`，两个「已经决定好了」的入口。
  不解析任何一行文本，一次性路径把 argv 原样递进来。
- `tui`：`submission()` 拿着输入行，决定它是哪一种。`/` 的去处、代词的展开、Tab 补全都
  留在这一层，因为它们是「输入框」的性质，不是「命令」的性质。

顺带把错法的回答做成诊断：带 `/` 或带路径分隔符的第一个 token，取出最后一段当命令名去目录
里查，命中就回一句「`/x` 是输入行语法，命令行直接写 `gongga x ...`」，并补齐剩下的参数。
Windows 上 Git Bash 会**在程序启动之前**把 `/x` 改写成 `C:/.../Git/x`，所以这条规则认的是
「以已知命令名结尾的路径」，而不只是开头的 `/` —— 否则模型看到的仍是一句无用的
`unknown command "C:/SoftWare/git/Git/get_messages"`。

### 命令（SPEC 的 5 条服务端命令 + `get_messages` + 2 条客户端命令）

服务端：`send_message --message <m> --session <s>`、
`set_session --session <s> [--cwd <path>] [--llm <provider/model>] [--context_limit <n%>]`、
`del_session --session <s>`、`interrupt_session --session <s>`、`cancel`、
`get_messages --session <s> --range <a:b>`。
- **会话名一律叫 `--session`**。以前 `set_session` / `del_session` / `interrupt_session` 写的是
  `--name`，只有 `send_message` 是 `--session` —— 同一个东西四种拼法，代价是 sticky
  `session` 只能填上其中三条。现在四条一致，`set_sticky_cmd_argument --name session
  --value main` 对每条命令都生效。
- `set_session` 同时承担创建与配置：命名是唯一得到会话的方式，拒绝创建会让它没用。
  **它不再设置任何「当前会话」** —— 想要默认值就用客户端的 sticky `session`。
  `--llm` 与 `--context_limit` 可以随时改，改的是**下一轮**用什么，不动正在跑的 turn。
- `send_message --session` 是**必填**的。缺了就在客户端被拒（目录里标了 required），
  服务端也再查一次，不给自己留隐式落点。
- `interrupt_session` 停**一个**会话的 turn，并且**丢掉**它没干完的活（连排在同一会话后面
  的 pending 行一起清）。它以前是 Ctrl+X，只有 TUI 能按 —— 于是脚本和模型都没法停掉自己
  起的 turn。既然停活本身就是一种请求，它就该是一条命令，和别的命令走同一扇门。
  用 `peek` 而不是 `get` 找会话：没在内存里的会话没有 turn 可停，为了停而给它起 shell 是笑话。
  空闲、不存在这两种情况都会明确回一句，否则「静默成功」读起来就像真的停住了。
- `cancel` 是 SPEC 那条「not instantly」的停服：先取消所有在跑的 turn、把没干完的活留在
  `pending_tasks` 里，等 250ms 让「正在取消」的回执发出去，再让监听循环退出，并给在跑的
  turn 最多 30s 落盘。下次启动 `resume_pending` 自动重放。
  **它不杀 detached 作业**（回执里会报「N 个还在跑，用 `del_bash` 停」）——
  那正是 detached 存在的理由，见上一节。
- `get_messages` 是**唯一的读命令**，也是 SPEC「structured for agentic retrieval」第一次
  真的被用上：`send_message` 是 fire-and-forget、`response` 是一堆交错的增量，两者都不适合
  「我不在场，告诉我它最后说了什么」。它取 `--range <a:b>` 这段**闭区间**的 seq
  （两端都含、负数从末尾数，`-500:` 就是尾巴），顺手把 `last_seq` 和会话 `status`
  带回来 —— 调用方据此推进游标，不许多跑一趟，也不会因为「这一轮只有工具调用」
  而判断不出该从哪继续。**只读就不排队**：它不拿会话门，因为读一份正在被追加的
  transcript 正是 TUI 回放在做的事，两者都没有东西要写。行本身按它当初的事件重发
  （`response`/`tool_answer`/`log`，`turn=0`），`data` 只留 `session`/`status`/`last_seq`——
  调用方推进游标要的是「读到哪儿了」，不是行。

客户端（本地执行，不上线）：`set_sticky_cmd_argument --name --value`、
`del_sticky_cmd_argument --name`，和 SPEC 那条
`set_event_ui [--block_size] [--session] [--split]`——三个可选参数一条命令。
另有 `set_event_filter --name --value` / `del_event_filter --name`：SPEC 现在只写了会话
过滤（`set_event_ui --session`），通用的按字段过滤先留着，和它共用同一份状态。

### 事件过滤器（`set_event_ui --session` / `set_event_filter`）

多会话时日志是交错的一条流，sticky `session` 解决的是「发给谁」，不解决「看谁的」。
过滤器补上后一半，和 sticky 参数一样存在 `~/.gongga/cli.toml` 的 `event_filters` 段：
SPEC 给这一半起的名字是 `set_event_ui --session <name|none>`，它写进的就是这里的
`session` 一条；`--name/--value` 那两条是能按任何字段收窄的通用形状，暂时不进 SPEC。
两个写入方各改各的段，互不覆盖（以前 `Sticky::save` 直接整文件重写，加一个写入方就会
把对方删掉，所以先改成「读盘 → 改自己那段 → 写回」）。

三条规则，都是为了让「过滤」不变成一个会咬人的开关：

- **每一对都要命中**。`session=main` 加 `part=output` 是「main 的回答」，不是并集。
- **事件没有这个字段就放行**。`session` 只约束会话流；`cmd`/`cmd_result` 是 operator
  自己敲的，不属于任何会话，滤掉会出现「只看到输出、看不到是谁发的命令」。
- **只滤行，不滤状态**。被滤掉的 `status` 仍然更新会话列表，`sessions` 事件永远不滤
  ——它是客户端学到会话名的唯一途径。把状态一起滤掉，界面上就是「另一个会话看起来死了」。

`--name` 只接受线上真实存在的字段（`EVENT_FIELDS`），不在表里的当场拒绝并列出可选项：
一个永远不命中任何事件的过滤器，只会让用户以为日志坏了。比较走序列化后的 JSON 字段名，
所以过滤值就是 `jq` 里会写的那个名字，新增事件变体也不用加分支。

`set_session` 成功（`cmd_result ok`）后自动把 sticky `session` 和 `session` 过滤器一起设为
该会话（SPEC 的两条 `- auto set/del --session`），`del_session` 只在删的正是过滤器指向的
那个会话时才清——删掉别的会话不该悄悄改掉「我在看哪个」。**不在提交时就改**：
`set_session --llm nosuch/model` 会被服务端拒，那时客户端已经指向一个不存在的会话，
表现为「日志忽然不说话了」。为此 `Client.follows` 记 (id, 命令, 会话)，收到回执才结算；
一次性模式也照此走，所以 `run_once` 现在把事件同时喂给 client（静默），stdout 仍只有 JSON。

非交互模式（`gongga <cmd>`）**不应用过滤**：脚本拿到的是它要的那些事件的完整 JSON，
而且滤掉 `cmd_result` 会让它一直等一个永远不会打印的回执。

## 5. CLI（命令层）

`cli` 持有三样东西：服务端下发的命令目录、本地的 sticky 参数（和 `connect_server` 记住的
地址，都在 `cli.toml`），以及内嵌的 server 库。它把输入行翻译成
`Submission::{Send, Local, Rejected}`，再通过 `Sink` trait 交给前端渲染 ——
TUI 是一种实现（带样式的行），非交互模式是另一种（什么都不渲染，只把事件打成 JSON）。

- **补全目录 = 服务端目录 + 客户端目录**。三个位置都补：命令名、`--` 后的参数名、
  参数值。值只在目录声明了来源时才补，来源有五种：`Sessions`（活着的会话）、
  `Static`（固定集合：`--llm` 的名单条目、`--context_limit` 的百分位）、
  `Arguments`（目录里所有参数名，给 `set_sticky_cmd_argument --name`）、
  `Paths`（客户端这台机器的文件系统，给 `--cwd`）、`OfNamedArgument`（见下）。
  `del_sticky_cmd_argument --name` 的候选来自当前 sticky 参数名，随 sticky 变化重建。
- **打了一半的值也要补**：`--session <TAB>` 与 `--session sc<TAB>` 是同一份候选。判据是
  「光标前一个词是不是旗标」，而不是「行尾是不是旗标」——以前只看行尾，于是打出几个字母
  之后就再也补不出来，等于只支持「一个字母都没打」的补全。
- **`--value` 的形状取决于它要填哪个参数**：`set_sticky_cmd_argument --name session
  --value <TAB>` 列会话名，`--name message` 就什么都不列。`OfNamedArgument` 的含义就是
  「读同一行 `--name` 的值，按那个参数声明的来源补」，所以不需要在每个命令上重复一遍。
- **目录不带尾随空格**：`apply_completion` 默认补完补一个空格（旗标后面要跟值、命令名
  后面要跟旗标），但目录补成 `src/ ` 之后，下一次 Tab 的「前一个词」就不是旗标了，
  于是再也下不去。`Candidate::space = false` 只给目录用，Tab 连按就能逐层往下走。
  点开头的条目默认不列（像 shell），除非前缀本身就是 `.`。
- **`--cwd` 列的是客户端自己的文件系统**：目录里没有这台机器的目录结构，而 Tab 是客户端
  在处理；两者在同一台机器上（同一个 `gongga` 进程旁边就是服务端），所以由客户端读盘比
  让服务端回一趟更直接。代价是相对路径仍由服务端按它自己的工作目录解析，两边不一致时
  Tab 出来的目录服务端可能找不到——写绝对路径就没有这个歧义。
- **输入行有两种写法，都直接来自 SPEC。** `/` 开头是命令：`/send_message --session main
  --message hi`。其余整行是消息，展开成 `send_message --message "<text>"`（SPEC
  「`<pure text>` expand to …」，代码在 `tui/src/run.rs` 的 `client.message()`）。
  这一版曾经写成「裸文本不再被当成消息」，那是错的：它描述的是更早的一个设计，而
  「粘一行字就能发出去」正是 TUI 存在的理由。`/` 开头但命令不认识时仍然拒绝并列出已知
  命令 —— 打错命令名和发一句以 `/` 开头的话是两件事，前者应该报错。
- sticky `session` 仍然生效，但只做 SPEC 说的那件事：**补齐命令里没写的参数**
  （`send_message --message hi` → 补 `--session`）。它不改变一行的含义。
- **命令不认识的参数直接拒绝**，并列出它能接受的。以前多余参数一路透传到服务端、
  那里也只看必填项，于是 `--name x` 在改名之后仍然「成功」：它什么都没填，sticky
  `session` 把真正的参数补上了，命令落到了调用方根本没点名的会话上。这类静默忽略
  比报错贵得多，所以现在在客户端就拦住。

TUI 和 CLI **共用这一套模型**，TUI 不持有任何额外状态：它只是 `Sink` 的一种实现。
状态条只报服务端事实（model、会话数量），**没有「当前会话」这一项**；sticky 参数单独一行
显示，那是用户显式设的 sticky argument，不是 UI 的模式。
`Sink` 的每条行和每个流都带一个**窗口名**：哪行进哪个窗口是语义（会话的事件归那个会话，
没点会话名的命令归操作者自己那一线），所以由命令层决定，并且**永远**这么标——公布给前端的
（`Sink::layout`）只说「这一帧怎么分屏」，不说「谁存在」。前端因此只有一份日志、多个视图。
**没有任何一条路径会替用户推断会话** —— 连以前那个 Ctrl+X 也没有了：`interrupt_session`
和别的命令一样要求 `--session`，想省掉打字就设 sticky `session`，那是显式的。

TUI 布局：头部状态条 / 事件窗口（分屏时是若干面板，每个上面一行写它是哪个会话）/
sticky 参数行 / `cmd` 与回执那一线（只在分屏时占一行，位于 footer 与输入框之间）/
输入框 / 帮助行，补全弹窗浮在输入框上方。
输入框是**多行**的，高度随内容变：超出行宽自动折行，`Shift+Enter`（或 `Alt+Enter`）插入
硬换行，行数上限 8 行 —— 到顶就不再长、改为内部滚动跟随光标，因为一个吃掉半屏的输入框
会把「正在回复的那段话」藏起来。折行是渲染算的，文本里只有用户自己打的 `\n`，
所以历史、提交、线上传输看到的都还是同一个字符串。
滚动：为算准「底部」位置，日志是**手工按显示宽度换行**后取可视切片，而不是交给
Paragraph 自动换行（那样拿不到换行后的总行数）。`scroll` 的含义是「距底部多少行」（0 = 跟着
最新一行），它的上界 `max_scroll` 由**渲染回写**——只有渲染知道换行后的总行数和窗口高度；
滚动键和渲染各 clamp 一次，所以行被删掉或窗口变高时视图不会停在空白处。分屏之后这套是
**每个面板一份**：`scroll`/`max_scroll` 长在面板上，键盘只作用于「焦点面板」。

三个显示设置是**一条命令的三个参数**，都是客户端状态（服务端发的事件一模一样）：
`set_event_ui [--block_size <line>] [--session <name|none>] [--split <bool>]`。
给几个参数改几个，一个都不给就是**只读**：打印当前三项。但**先全部解析、再全部落盘**——
`--block_size 9 --split sideways` 整条被拒，不会留下半套配置，也不会顺手报告它没动过的
那一项；而不管写入还是读取，答案都是完整的一行 `block_size = 3  session = main  split = false`，
因为一条命令给三个答复就是让人自己拼。

- `--block_size <line>`：一个块最多占几行，超出的**跟着尾部走**——块显示它最新的几行，
  更早的行不再进滚动范围（全文在库里，用 `get_messages` 读）。`0` 就是不限高。SPEC 的措辞是
  "max line count of a **non-output** block"：助手的回答不受限，因为它是操作者来读的东西，
  其余（thinking / tool call / 工具输出 / 命令回显）都是进度，值得收。这一条取代了原来的
  `set_event_detail --mode all|line`：一个数字不必记词汇，而旧的「折成一行且固定头部」在真实
  流式里读起来像一行在原地不动、字从右边漏掉，`auto scroll` 说的是相反的那件事。它带来的
  `FOLD_KEEP` 字节上限也一并删掉（AGENTS.md 禁止设上限）。取消限高是 `--block_size 0`，说出口。
- `--session <name|none>`：只渲染那个会话的事件行；`none` 是「全都渲染」那个词，不是有个会话
  叫 none。它写的就是 `event_filters` 里 `session` 那一条，和 `set_event_filter --name session
  --value …` 同一个存储，所以两种拼法不会为「屏幕上是什么」各说一套。分屏时**面板跟着行一起
  收窄**：过滤到 main 却给 peer 留一块空面板，是操作者没要过的布局。这是**渲染的过滤**，不是
  接收的过滤——事件照旧到达、状态照旧更新（被隐藏的 `status` 仍然刷新会话列表），只是不画。
- `--split <bool>`：events 区按会话名切成独立面板，各自滚动、各自贴自己的最新行。分屏是**视图**
  的切换而不是数据的搬迁：一条行只存一份，合并视图与各个会话视图是同一批块的两读法，所以切来
  切去历史都还在（先做成「每个面板一份日志」时被真终端帧打回来了：切过去之后每个面板只剩切换
  之后到达的行，还在流的那条从半句话接起来）。带了 `--session` 的命令（回显、读回来的行、回执）
  进那个会话的面板；只有没点会话名的命令走 footer 与输入框之间那一线，且那一线每块固定一行。
  会话恰好叫 `cmd` 时命令行窗口往后加一个空格，不会被会话抢走。

`set_session` 从此**不再改动渲染**：它只把 `session` 记成 sticky 参数（决定「发给谁」），不管
「在看谁」。两处一起变了：以前那条「`set_event_filter` auto set `--session` after `set_session`」
随命令合并从 SPEC 里消失；而 sticky 现在**只填必填参数**——`set_event_ui --session` 是可选的，
被 sticky 填上就等于「改一下块高，顺带偷偷换了你在看哪个会话」。要看谁的会话，说出口。

翻事件历史的键位：↑↓ 先归补全弹窗、再归输入框里的上一行/下一行、最后才是命令历史
（shell 习惯，也是人手先试的那两个）；←→ 是光标。所以翻页落在 **Alt+↑↓（一页）/
Shift+↑↓（一行）** 上，PgUp/PgDn 同样翻页但在笔记本上藏在 Fn 后面。**鼠标滚轮一次 3 行、
按住 Shift 一次 1 行**：为此进 alternate screen 时必须开 mouse capture（退出时关掉），
否则滚轮事件根本到不了进程，终端自己留着也没处滚。分屏时滚轮**指哪个面板就滚哪个，并把它
设为焦点**——焦点面板的表头反色显示，键盘翻页只作用于它；面板多于行数时画不下的面板不画，
最后一个表头写 `+N not shown`。

**退出只有 Alt+Q**，而且**所有修饰键绑定都在 Alt 上，不在 Ctrl 上**。原因是宿主而不是
哲学：VS Code 的终端会先把 `Ctrl+Q`（以及 `Ctrl+K`、`Ctrl+←/↑/↓`）当成自己的快捷键吃掉，
进程根本收不到 —— 一个按不出来的绑定等于没有绑定。Alt 组合经 ESC 前缀到达，终端普遍不拦。
（原来还有 `Ctrl+Q` 走 XON/`IXON` 的考虑，现在不需要了：Ctrl 不再是任何绑定的修饰键。）

**`Shift+Enter` 的前提是终端肯报修饰键。** 不报的终端（xterm.js 系默认如此，包括 VS Code）
把 `Shift+Enter` 和 `Enter` 都发成 CR，两者在进程里无法区分 —— 那时用 `Alt+Enter`，它走
ESC 前缀，一定分得出来。程序能做的是把两种都接住，做不了的是替终端撒谎。

Ctrl+C 和 Ctrl+X 都删了：一手 `Ctrl+C` 的老习惯在 TUI 里意味着「撤销」，而这里的撤销根本
不打算撤销，它要的是一个 `interrupt_session` 命令。留下一堆「哪个键偏疼哪件事」的约定，
最后没人记得住；现在键盘只负责行内编辑、翻页、补全和退出，所有会改变服务端状态的动作都是
一个能补全、能进历史、能被脚本复制的命令。

### 断线与重连

服务端和客户端互不依赖。**链路是客户端的一种状态，不是它存在的前提**：TUI 先开窗口、再在
循环里拨号，所以服务端还没起来时也能先开客户端；服务端重启时窗口不关，只是回到拨号。

- 链路读端给出 `None`（channel 结束，或 ICMP 把 UDP socket 打醒让读端报错）→ 打一行
  `server connection lost — reconnecting`，状态条从 `model … | N session(s)` 换成
  `offline — reconnecting (attempt N)`。**它不再 `break`**：输入行、命令历史、滚动位置都是
  客户端自己的状态，一行都不动。
- **重连就是重新握手**，没有第二条 resync 路径：`Hello` 回来时照原样把 catalog / sessions /
  model 刷一遍 —— 同一条消息既开始了新会话，也恢复了一个旧窗口，这正是重连便宜的原因。
- 退避：250ms 起、每次失败翻倍、封顶 5s（`RECONNECT_MIN` / `RECONNECT_MAX`）。
- **拨号放在 `select!` 里和键盘并行**，不是 `await` 在分支里。握手超时是 5s；直接等的话，
  服务端一掉线窗口就每隔几秒僵 5 秒 —— 恰恰是最想按 Ctrl+Q 的时候。拨号的 future 跨迭代
  存着（`Box::pin`），按键不会把它打断，也不会让它被反复重启。
- 断线时打的命令：**客户端命令（sticky）照常本地生效**，它们本来就不出进程；服务端命令
  不发，原样**放回输入行**（`Submission::Offline`）。那行根本没离开过进程，不该被一条日志
  吞掉，也不该假装发过。
- **不重连的错误**：密钥被拒、地址解析失败。问第二遍答案一样，重试只会把真正的原因埋进
  一屏 `reconnecting` 里，所以直接退出并报原因。
- **一次性模式不重连**：脚本/模型的一条命令，等不到回答就是失败。重连会把「没发出去」和
  「不知道发没发出去」混成一件事。
- 服务端侧无需改动：新链路回的是**新本地端口**上的 SYN，`peer` 地址天然是新的，所以旧连接
  不会被误用（见 §8 第 19 条：旧条目要等到有包从那个地址来才会被清掉）。

### 模型请求的超时与重试

一个「连上了但不说话」的 provider 曾经能把 turn 永久挂住：没有错误、没有 EOF、没有 socket 可
查，谁也叫不醒。现在它被两个**知道自己量什么**的计时器夹住，外加自动重试：

- `headers_timeout_ms`（默认 180s）：从发出请求到收到响应头，上传算在内。它只管「答不答」，
  **不碰响应体**。
- `idle_timeout_ms`（默认 60s）：**内容**的静默上限 —— 只有真正的增量才重置它，keep-alive /
  注释块这类只有字节没有事件的 chunk 不算。
- **没有覆盖响应体的墙钟。** 以前有一个（180s，流式算在内），它会把正在输出的长回答拦腰砍断：
  实测单轮响应 p99 132s、最长 470s（46.8k 输出 token），而 8 次中断全部精确落在发出后 180.0s。
  回答的长度由 provider 自己的输出上限约束，不由 socket 计时器约束。
- 连接本身另有 30s 的 connect timeout（`CONNECT_TIMEOUT`，不可配）。
- `max_retries`（默认 0 = 不限）/ `retry_base_ms`（默认 1000）：失败、超时、或**完成了但一个字
  都没有**的空响应，以及**读流读到一半 panic**，都会退避重试，延迟翻倍（1s → 2s → 4s，封顶
  30s）。
- 只有「本次尝试一个字都没说」才允许重试：已经说过话再问一次，会给出第二个互相矛盾的答案。
- 重试会发一条 warn 到 TUI（`retry 1/3 in 1s`），所以慢的时候看得到是在重试，而不是在思考。

`GONGGA_LLM_*` 环境变量可覆盖配置项。

**卡住不等于网络问题。** 2026-10 那次「agent 卡住」排查下来是两件事叠在一起，都不在网络上：

1. 压缩提示词里的 `clip()` 按**字节**截断（`&text[..max]`），中文一出现就 panic。turn 的 task 直接
   死掉，而清理状态的代码在它后面 —— 于是会话永远停在 `running`：没有请求在飞、没有 CPU、
   没有日志，和「provider 不答了」一模一样。现在按字符切（`char_indices`），并有单测守着。
2. turn 现在整体包在 `catch_unwind` 里：再 panic 也会落到 `Error` 状态并打一条 error 日志，
   而不是永远 running。
3. 那只解决了「永远 running」，没解决「不重试」：panic 在 `stream_once` 自己的 task 里，把 event
   channel 一起带走，外层只看见一条「停了但没完成」的流 —— 没有 `Failed` 可据此重试，也没有人说出
   原因。现在**每次尝试各自包一层** `catch_unwind`，panic 就是那一次尝试的失败，回到本来就有的重试环；
   一条既没 `Completed` 也没 `Failed` 就结束的流同样自己报上原因。能不能再试一次，仍然由第 4 条
   「本次尝试一个字都没说」决定。

排查这类问题的抓手是分段日志（INFO 级，带 elapsed）：`turn start/end`、`step start`、
`tool start/end`、`model request: sending/headers/stream ended`、`compression request done`。
看到最后一条是什么，就知道卡在哪一段。

### 非交互入口

```sh
gongga set_session --session demo                    # 命令本身就是子命令
gongga send_message --message "hi" --session demo    # 拿到回执就退出
gongga "set_session --session demo"                  # 整行形式，脚本里拼出来的也能用
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

### 谁起服务端

SPEC 的结构是：`server` 是**库**，`cli` 是**二进制**并内嵌它。于是「起一个服务端」不是某个要另外
安装的程序，而是这个二进制自己的一个模式 —— 客户端命令 `serve`：

```sh
gongga --home ~/.gongga serve --port 9700
```

它是**客户端命令**而不是隐藏开关：命令层本来就是「这个程序能被要求做什么」的清单，`serve` 因此
照常出现在目录里、能补全、能自我说明。它在 `cli::main` 里于任何 tokio 运行时建立**之前**被分派，
因为服务端要自己建它想被停掉的那个运行时（`server::main`），嵌套一个会 panic。

**谁自动起**：只有窗口。SPEC 把它写给了 `tui`，同时给了 `--client_only` 说「别起，我自己有一个」。
一次性命令**不**自动起 —— 它是脚本或模型在跑，一个脚本悄悄在别人端口上拉起守护进程是惊吓不是便利。
窗口在**第一次拨号失败**时起（`attempts == 1`），而不是启动时：那一刻正是操作员知道「没人应答」的
时候，也顺带覆盖了 `connect_server` 指向一个没人起的服务端，`--client_only` 只有一处需要说「不」。
起服务端是 spawn 出去的任务而不是当场 await，所以窗口在「第一次启动要迁移 home、拉 content」的
整个过程中仍然收键盘。

三条判断写在这里，因为每一条都可以反过来：

- **只有本机地址才起。** 拨 `10.1.2.3:9700` 失败是别人的服务端断了，答案不是给这个 home 起一个
  —— 那会是一个没人要的服务端，绑在没人听的端口上。`is_loopback()` 是这条唯一的判据。
- **由一次真实拨号判定，不看 `server.lock`。** 锁里是 pid 不是地址，而一个 home 可以被配置绑在任意
  端口；「有没有人在」只有链路能回答。它也顺手覆盖了最要紧的一种：用错 key 答话的服务端是**在**的，
  这时再起一个就是这个设计最反对的「一个 home 两个服务端」。
- **被拒绝也算应答。** `DialFailure::Fatal` 意味着有人答话了并说了不 —— 它是**在**的，再起一个只会
  和正在服务的那个抢锁，输的那个唯一的下场是一行没人看的日志。

起出来的进程**不属于调用者**：自成进程组（Windows 上是 `CREATE_NO_WINDOW`），输出追加进
`<home>/server.log`。这里踩过一次坑：`DETACHED_PROCESS` 看起来更彻底，但「没有控制台」会让
Windows 给**每个**控制台子进程开一个新窗口 —— 而服务端启动时要跑 `git`（取 ship 内容）、
`python`（迁移脚本）、`bash`（会话的 shell），于是满屏弹窗。`CREATE_NO_WINDOW` 给的是一个存在
但永不显示的控制台，子进程继承它。一条一次性命令几百毫秒就退出了，服务端要是跟着它的终端一起死，
刚才那个 turn 也就白跑了。`--home` 显式传给子进程：它要服务的是刚才写了 shim 的那个 home。

**不等它起来。** 窗口本来就在退避重连，再挂一个「等到 ready」的循环只是把同一个循环写两遍，错误
还不如它。起服务端要负责的是「把它拉起来」，不是「盯着它」。

`<home>/bin/gongga` 这个 shim 归 CLI 写（`cli::server::install_command`）：它知道自己在哪。服务端
只把 `<home>/bin` 排到 `PATH` 首位（`server/shell_command.rs`），两边只需要对「home」有共识 ——
服务端不再搜索 CLI 二进制。shim 写不出来时（找不到命令层二进制，比如 `dev.sh` 只 stage 了一个
改名副本），服务端**不带** `GONGGA_CLI` 并 warn 一句，而不是指向一个不存在的东西。

shim 写的是**命令层**，不是当前跑着的那个程序：`gongga-tui` 也内嵌 `cli`，agent 敲 `gongga`
不该拿到一个它看不见的窗口。写入走「先写旁边的临时文件、再 rename」，因为 agent 的 shell 随时
可能在读那个脚本 —— 半个 `#!/bin/sh` 是一个上一秒还好用的文件里的语法错误。

### TUI 的滚动缓冲：一帧只碰一屏

症状：对话一长，TUI 卡的要死 —— 不是「有点顿」，是打字都跟不上。

原因在渲染路径上，和网络、和模型都无关。旧的 `draw_log` **每帧把整个日志重新折行**：

- `wrap()` 用 `ch.to_string().width()` 量一个字符的宽度 —— **每个字符一次 `String` 分配**，
  5000 行正常长度的日志约 40 万个字符，即每帧 40 万次分配；
- 折完再 `text.clone()` 出可见行，又是一次全量复制；
- 而流式回答每来一个 chunk 就是一次 `Step::Wire` → `next_event` → 一帧。

于是成本是 O(对话长度) × O(chunk 频率)：对话越长帧越贵，而长对话恰恰是 chunk 最密的时候。
这就是「长了之后卡」的完整解释。

现在一帧只碰一屏（`crates/tui/src/log.rs`），四件事各管一段：

1. **每行缓存折行起点**（`Entry::starts`，每行一个 `Vec<usize>`）：画一行是切一段 `&str`，
   既不重新折行，也不复制文本 —— `Span::styled(&'a str)` 直接借用日志里的字节。
2. **追加接着折**：流式 delta 从最后一行的列号继续那台状态机（`Entry::open`），所以一个 chunk
   的成本是 chunk 的长度，不是这一行的长度。没有这一步，一条长回答就是 O(长度²)。
3. **行号 → 行索引用 Fenwick 树**（`RowIndex`）：`window()` 要知道「第 N 个折行落在哪一行」。
   前缀和数组在纯追加下是对的，但流式中途会改**中间**那一行的行数（工具输出和回答交错），
   它之后的所有前缀和全部失效；树把改和查都做成 O(log 行数)，滚到顶和滚一格一样快。
4. **帧有脏标记**：什么都没变就不写终端。流式比终端刷新快的时候，这挡掉的正是最贵的那部分。

唯一要走全表的是**宽度变化**（resize）：每个缓存偏移都是按旧宽度量的，只能重排一遍
（O(总字节)，不分配字符串）。拖动窗口时会有几次，平常滚动一次也没有。

实测（`crates/tui/examples/frame_cost.rs`，release，80 列 × 24 行，每帧取 200 次平均）：

| 日志行数 | 旧 | 新 | 新（滚动到中部） |
|---|---|---|---|
| 100 | 140µs | 204ns | 156ns |
| 1,000 | 345µs | 149ns | 143ns |
| 5,000 | 1.9ms | 169ns | 166ns |
| 20,000 | 8.1ms | 156ns | 168ns |

新路径与日志长度无关（滚到中部多出来的只是 O(log n) 的树查找），旧路径线性增长 —— 20k 行时
一帧 8ms，配上每秒几十个 chunk，就是「卡的要死」。单测 `a_frame_touches_only_a_screenful`
守着这条不变量：5 万行时一帧也只能产出 24 行。

## 6. 配置

`~/.gongga/config.toml`（`--home` / `GONGGA_HOME` 可改），首次启动自动写一份带注释的模板。
所有字段都能被 `GONGGA_*` 环境变量覆盖。

```toml
[server] bind = "127.0.0.1:9700"  db = "~/.gongga/gongga.db"  key = "<32 hex>"
[llm]    provider = "deepseek-official"   # 名单条目；省略则用名单里第一个能用的
         model = "deepseek-flash"          # 省略同上
         base_url = "https://api.deepseek.com/v1"
         api_key_env = "MINIMAX_CN_API_KEY"
[tools.bash] timeout_ms = 120000  max_output_chars = 8000
[agent]  compress_above_tokens = 40000  keep_recent = 30   # 无步数上限
[transport] mtu = 1200  window = 64
```

`[llm]` 这一段**基本不用写**：provider / base_url / key / 上下文都来自 `~/.agents/llm.toml`
（见 §4「名单」），这里只用来**覆盖**它。`GONGGA_LLM_CATALOG` 可以把名单指到别的路径。

库路径也能在启动时直接给（SPEC「database path can be specified at server start」）：

```sh
gongga --home /tmp/scratch serve --port 9700     # 另一个 home，整个状态都在它下面
GONGGA_DB=:memory: gongga serve --port 9701       # 一次性库，碰不到真实数据
```

`--db` 在**写完默认 config 之后**才生效：这次顺手生成的 `config.toml` 记的是 home 下的默认路径，
不会被一次性的 `--db` 烤进去、再让以后每次启动都跑去那个路径。

`max_output_chars = 0` 关掉输出上限，`compress_above_tokens = 0` 关掉压缩。

CLI 的 sticky 参数单独放 `~/.gongga/cli.toml`，和服务端配置分开 —— 它们是客户端状态，
服务端不该看见。

Git Bash 自动探测常见安装路径，找不到再退回 PATH 上的 `bash`。

## 7. 验证情况

| 项目 | 状态 |
|---|---|
| `cargo build --workspace` | 通过 |
| `cargo clippy --workspace --all-targets` | 无警告 |
| 单元 + 集成测试（190 个） | 通过 |
| 真实 UDP：200KB 分片往返 / 25% 丢包恢复 / 顺序性 | 通过 |
| 端到端冒烟（**真实 MiniMax-M3.1-Flash** + 真 Git Bash） | 通过 |
| 端到端冒烟（假 LLM + 真 Git Bash，`detached` 工具调用） | 通过 |
| key 鉴权：错 key 被拒 / 对 key 放行 | 通过 |
| 命名 bash + `del_bash` | 通过 |
| `background` 作业输出由 `check_bash` 取走（真 Git Bash） | 通过 |
| `detached` 立刻返回 + `cancel` 能杀掉（真 Git Bash） | 通过 |
| sticky 参数客户端持久化 + 自动补齐 | 通过 |
| `send_message` 缺 `--session` 被拒 / 有 sticky 才补齐 | 通过 |
| 裸文本按 SPEC 展开成 `send_message --message`；`/` 开头才是命令 | 通过 |
| `/` 只在输入行里：命令行是 argv，不带 `/`、不做纯文本展开 | 通过（`a_leading_slash_is_refused_with_the_command_line_it_meant`、`a_shell_rewritten_slash_is_recognised`）|
| 非 UTF-8 输出不会让 shell 变哑：读字节、丢哨兵即换 shell | 通过（`binary_output_does_not_deafen_the_shell` 原样复现 `cat` 二进制后 `echo`；`a_shell_that_ended_is_not_reusable` 管 EOF）|
| 拒绝信息由 schema 生成（`argument_error`）：列出全部参数/类型/required，缺 required 才拒 | 通过（单测 `a_call_missing_a_required_argument_is_answered_with_its_own_schema`）|
| `bash` 参数为 `command`；`cmd` 这类缩写会被指回 `command` | 通过（同上，及 `bash_takes_name_command_and_the_two_detach_flags`）|
| 状态条不含「当前 session」 | 通过 |
| `cancel`：回执发出 → 停服 → 进程退出 | 通过 |
| `interrupt_session`：只停一个会话、pending 行被清（`cancel` 则保留） | 逻辑与目录有单测，**未跑真 provider** |
| 退出键：Alt+Q；Ctrl+Q / Ctrl+X / Ctrl+C 不再有任何绑定 | 逻辑简单（单测钉住键位），**真 TTY 未验证** |
| 服务端重启：TUI 不退出，退避重连、重新握手刷回 catalog/sessions | 逻辑完整、编译通过，**真 TTY 未验证** |
| 断线时打的服务端命令被放回输入行（不发、不算错） | 逻辑完整，**真 TTY 未验证** |
| detached 作业**跨服务端退出存活** | 通过 |
| detached 作业被下一个服务端 `adopt()` 接管并仍可杀 | 通过 |
| `adopt()` 的开销与行数无关：一次 `tasklist` 回答全部 pid | 通过（156 行陈旧记录：修复前 75.4s 才 bind，修复后 1.4–1.9s；20/50/156 行基本持平） |
| reasoning `summary` 从 delta 累积（真实 provider：0 → 395 字符） | 通过 |
| 回传的 `encrypted_content` 被 provider 消费（真 blob 答对 / 垃圾 blob 答不出） | 通过 |
| 名单自动导入：`~/.agents/llm.toml` 15 个 provider，启动即用 | 通过（`llm roster imported ... providers=15`）|
| 无配置时按名单选默认条目：`deepseek-official/deepseek-flash`，`/v1/responses` 可用 | 通过（真实 DeepSeek，609ms 一轮）|
| `set_session --llm`：`deepseek-official/deepseek-v4-pro` 一轮答对；`minimax-cn/MiniMax-M3.1-Flash` 一轮答对 | 通过（两条都真跑，密钥链各自命中）|
| `set_session --context_limit 30%`：日志 `budget=300000`（1M × 30%，单位是 token） | 通过 |
| `--llm` 打错：列出 15 个可用 provider | 通过 |
| 老会话不被默认值带走：`test1` 仍按 `minimax-cn/MiniMax-M3.1-Flash` 路由 | 通过（`llm` 列为空时用 `model` 反查名单）|
| 中断在工具调用中间的 transcript 能继续用：造一条无结果的 `tool_call` 再追问 | 通过（真实 DeepSeek 答「结果未知，本轮被中断」；修复前同一份历史整条 400）|
| `--name` 被拒并列出 `--session/--cwd/--llm/--context_limit` | 通过 |
| 值补全：`--session` 列会话、`--llm` 列 44 个名单条目、`--context_limit` 列 0–90% | 通过（单测 5 个 + 目录过线测试）|
| 值补全：`set_sticky_cmd_argument --name` 列全部参数名；`--name session --value` 列会话 | 通过（单测）|
| 值补全：`--cwd` 列目录，目录补成 `src/` 不带空格、再按 Tab 能进下一层 | 通过（单测，真 TTY 未验证）|
| 工具输出上限：22KB `cat` → 2004 字符 + 落盘（真实 provider） | 通过 |
| 摘要压缩：第 51 条处压出 2318 字符摘要，会话继续（真实 provider） | 通过 |
| 模型从 bash 里调自己的 CLI（`$GONGGA_CLI cancel`，真实 provider） | 通过 |
| CLI 裸命令 / 整行引号形式两种写法，输出恒为 JSON lines | 通过 |
| `send_message` 拿到 ack 就退出（实测 0.45s），turn 在后台跑完才落库 | 通过 |
| `cancel` 后 pending 行**保留**，下个服务端 `resumed=1` 重放（真实 provider） | 通过 |
| `set_session` / `del_session` / 不存在的会话报错 | 通过 |
| `get_messages`：真实 66 条 transcript 上 `--after_seq 64 --limit 5` 只回 seq 65、66，`last_seq=66` | 通过 |
| `get_messages`：空窗口仍报 `last_seq`（游标不许倒退） | 通过（单测） |
| `bridge`：QQ 通道解码（@ 提及剥离、群/单聊事件、非消息事件忽略）、发消息类型、配置模板、`/stop` | 通过（单测 17 个） |
| `bridge` 启动缺凭据 / 白名单为空 | 通过：写模板 → 警告 → 明确报错退出 |
| `bridge` 端到端（真 QQ 沙箱收发） | **未验证**（需要 AppID/AppSecret 与沙箱群） |
| TUI 交互输入、补全弹窗 | **未验证**（需要真 TTY，只能人工开一次） |
| 滚动：滚轮 / Alt+↑↓ / Shift+↑↓ / PgUp/PgDn，两端 clamp | 逻辑有单测，**手感未验证** |
| TUI 渲染整链路（`TestBackend`，不需要 TTY）：布局、窗口、最新行在 footer 上方、按**行**滚动 | 通过（2 个单测） |
| 输入框折行：超宽自动换行、行数随内容变、上限后内部滚动、`\n` 是硬换行、光标落在正确的行 | 通过（`TestBackend` 3 个单测 + `layout_input` 断言）|
| `Enter` 发送 / `Shift+Enter`（`Alt+Enter`）换行 | 判定逻辑有单测，**真 TTY 未验证**（见下）|
| 补全：已填的 `--flag` 不再列出；Tab 完 flag 自动接上它的值列表 | 通过（catalog 单测 3 个 + tui 的 `apply_and_continue` 单测）|
| 长日志的一帧成本与日志长度无关（20k 行 156ns / 旧 8.1ms）| 通过（`examples/frame_cost`）+ 单测 `a_frame_touches_only_a_screenful` |
| 流式追加 = 整行重排（折行偏移一致）| 通过（单测 `appending_equals_rewrapping_from_the_top`，9 组 × 6 种宽度）|
| `--db <path>`：覆盖 `[server] db`、自动建目录、`:memory:`、不写回 config.toml | 通过（单测 + 真启动两次：`db=:memory:`、嵌套新目录被创建）|

端到端实录（真实 provider，会话 `demo`）：reasoning 流式 → tool-call 参数流式 →
`bash` 真实执行 `echo hello-from-gongga && pwd` 返回
`hello-from-gongga\n/c/resource/gongga` → 第二轮拿到最终回答。
另一次让模型用 `name:"build"` 开第二个 shell 再 `del_bash` 删掉，服务端日志确认
`shell started bash=build` 且模型随后正常收尾。

最近一轮（假 LLM + 真 Git Bash，端口 9798/9800）：`set_session` → sticky `session=main`
→ `send_message` → 模型调 `bash{"cmd":"sleep 12; echo survived > flag","detached":true}`
**没有阻塞** → 立刻 `cancel`（回执：`cancelling 1 session(s); 0 pending task(s); 64 detached
job(s) keep running -- they outlive the server, stop them with del_bash`）→ 服务端进程退出、
端口释放；**12 秒后 flag 出现**，说明那个进程确实跨过了服务端的死亡。
同一库再起一个服务端（9801），启动时清掉已结束的行、接管仍在跑的（该次 64 行 → 31 行被接管）。

## 8. 遗留问题（需要你定）

1. **UDP 只鉴权不加密**（见 §2）：流量内容对同网段可见。要不要上 DTLS / 换 QUIC？
2. **`response{session,...}` 的语义**（见 §3）。
3. **是否需要给 agent 暴露 DB 工具**（见 §4）。
4. **bash 零审批**下要不要危险命令黑名单（见 §4）。
5. **非交互输出只有 JSON**（见 §5）：人直接敲 `gongga set_session --session x` 看到的是一行
   JSON 而不是「session is now x」。TUI 才是给人看的，命令行是给脚本和模型看的 —— 是否
   要留一个 `--text` 兜底？
6. **同一会话并发**：现在新 turn 会取消旧 turn；并行工具调用是串行的。是否符合预期？
7. `set_session` 目前带了个 SPEC 没写的 `--cwd` 参数（新建会话时指定目录）。要不要去掉？
8. **两个「取消」现在是两条命令，语义不同**（见 §4）：`interrupt_session --session <n>` 丢活，
   `cancel` 留活并停服。刻意保持分开 —— 「这条别干了」和「停一下，重启后接着干」不是同一个
   请求。要不要把它们并成一条带 `--keep` 的命令？（不再有 Ctrl+X 这条旁路。）
9. **`detached` 不再跑在常驻 shell 里**（见 §4）：它拿不到 shell 的 export / 函数，
   也不能用 `check_bash` 收输出，只能读日志文件。这是「能跨服务端存活并可被叫停」换来的
   代价，是否接受？
10. **`del_bash` 是 SPEC 之外新增的第 5 个工具**（`bash` / `check_bash` / `list_bash` /
    `del_bash` / `del_bash`）。没有它 detached 作业就没有任何结束方式；但要不要改成
    `del_bash{name, job}` 这种「复用已有工具」的形态？
11. 仓库还没 `git init`。
12. **模型停自己这件事，现在是命令而不是快捷键**（见 §4）：`interrupt_session` 是服务端命令，
    模型从 `bash` 里敲 `$GONGGA_CLI interrupt_session --session <自己>` 就能停；`cancel` 同理
    （`test1` 当年是用 `taskkill` 硬停的，现在没必要）。要不要**进一步**在工具层加一条豁免
    ——比如禁掉 `$GONGGA_CLI cancel`，免得 agent 一顺手把整个服务端关了？
13. **`detached_jobs` 里那条 job 就是服务端自己**（`test1` 的 pid 31984 是 `exec` 出来的
    新 server 的 MSYS wrapper）：`del_bash` 一下就能自杀，而且重启 adopt 时有 pid 复用
    误伤的风险。要不要在 adopt / kill 时跳过「命令看起来是自己」的作业？
14. **压缩后 reasoning 链可能断**（见 §4 闸二）：保留区间里的加密块引用的前文已经变成
    摘要，provider 是否还消费它，目前没有直接证据。
15. **一次压缩 ~32s**：实测压缩请求 32s（2000 token 上限、非流式）。它不是每步都发生 ——
    `context()` 统计表里全部消息，而 `choose_cut` 要求 live 多于
    `keep_recent`(30) 条才切，压完 live 就只剩那 30 条，闸门直接关上（实测压完后再发消息：
    `step start` 前没有任何 compression，turn 1.1s 结束）。
    曾经浪费一次：启动 resume 的 turn 和新发的 turn 各自判了一次超预算，同一份上下文被压
    两遍、64 秒。**已解决** —— 同会话的 turn 现在排队（见 §4「冲突的操作排队」），第二个
    turn 开始时会看到第一个压好的摘要。
16. **压缩期间不吃 cancel**：turn A 被新 turn 取消后，它那次 32s 的压缩照样跑完才退出
    （日志里 `turn end outcome=Cancelled` 出现在压缩之后）。因为请求期间没有 select 取消信号。
    要不要让压缩也能被打断？
17. ~~**排队的 turn 没法单独取消**~~ **已解决**：`interrupt_session --session <n>` 是服务端命令，
    脚本、`$GONGGA_CLI interrupt_session --session x`（也就是模型自己）和 TUI 打的是同一条。
    它连排队中的 pending 行一起清 —— 排队中的 turn 放行时会看到 epoch 变了而直接走人。
18. ~~**`del_session` 会一直等到 turn 结束**~~ **已解决**：删除现在**先 `interrupt` 再拿门**
    （见 §4「冲突的操作排队」）。实测在跑 `sleep 45` 的会话上删除：0.49s 返回，bash 在
    11.6s 处被中断。它同时说明 `interrupt_session` 是能打断正在执行的 bash 的，不只是
    「等模型」那段。
19. **重连会在服务端留下一份旧 peer 条目**：`serve` 的 `conns` 只在「往那个地址发包失败」时
    才删，而客户端重连后换了本地端口，旧地址再也不会来包，于是每重启一次留一条（内存极小，
    但确实是只增不减）。要不要让 `handle_connection` 结束时通知主循环清掉自己那一项？
20. **`get_messages` 不在 SPEC 的命令表里**：SPEC 的 §Cmd 列了 4 条服务端命令 + `cancel`，
    没有 `get_messages`。它是 IM 桥接的读出口（见 §9），删掉桥接就得从 `response` 增量里
    自己拼答案。要不要保留并写进 SPEC，还是让桥接改用别的读法？
21. **名单里没写 `endpoints` 的模型一律当作支持 `/responses`**：实测 `deepseek-official`、
    `minimax-cn` 都能用（真的跑通了），但一个只支持 `/chat/completions` 的模型会在请求时
    拿到 404，而不是在 `set_session --llm` 时被挡住。要不要改成「只有明确列了 `/responses`
    的才允许 `--llm`」？
22. **provider 不返回 `usage` 时压缩不会触发**：兼容层若省掉这个字段，`context_tokens`
    永远停在 0，上下文只增不减，直到某次请求被 provider 以超长拒绝。这是刻意的取舍 ——
    宁可暴露在 400 上，也不用一个假比率把上下文撑爆。要覆盖它，得让名册声明「这家报不报
    usage」，再把按字符估降级成显式的兜底。
23. **`--cwd` 的补全是客户端的目录，执行却是服务端的目录**：两边是同一台机器，但
    服务端可能从别的目录启动，于是 Tab 出来的相对路径它可能找不到。写绝对路径
    就没有歧义；要不要让补全直接把相对前缀展开成绝对路径（更安全，但输入行会变长）？
24. **补全候选没有分页**：`Static` 的 `--llm` 已经是一份 44 项的名单，`--cwd` 更是整个
    目录。弹窗现在是「列全部、靠前缀收窄」，没有翻页/分栏，输入行为空时确实会顶掉大半个
    屏幕。要不要给候选加上限（比如 20 项 + `…`），或者按 usage 排序？
25. **`keep_recent` 在极端情况下会让压缩失效**：`choose_cut` 要求至少留下 30 条（可配），
    当活着的消息**本身**就不足 30 条却已经超预算（几条巨大的工具输出），它会返回 `None`，
    于是既不压也不再有机会压，下一步的请求就有可能被 provider 以超长拒掉。改成「超预算时
    压掉一半」是对的，但那会动到一个已经通过的行为（短 transcript 不压），需要连带改测试和
    语义，所以先记在这里。

## 9. IM 桥接（`crates/bridge`，二进制 `gongga-bridge`）

目标：在聊天软件里说一句话，等于在 TUI 里敲一行 `send_message`。**服务端一行没改**（除了
§4 那条只读的 `get_messages`）。

### 为什么是客户端，不是插到服务端里

三条既有不变式决定了这件事只有一个落点：

| 不变式 | 如果改成「服务端内置 IM」会怎样 |
|---|---|
| server 是**唯一的 DB 写者**，一切经由命令 | 桥接器直接写 `messages` 表，`pending_tasks`、压缩游标、事件广播全部绕开 |
| 一个 home 只允许一个服务端（`server.lock`） | 每个 IM 通道都要挤进那一个进程的生命周期，连不上就等于 agent 挂了 |
| 事件 `bus` 广播给**每个**已连接客户端 | 本来就能免费拿到：桥接器和 TUI 同时连着，你在终端看得见微信/QQ 那头在让 agent 干什么 |

所以 `bridge` 和 `cli`、`tui` 平级：同一把 key、同一次握手、同一份命令目录。

### 通道拆成 Inbox / Outbox

不是风格问题，是借用检查器逼出来的：一个 `&mut self` 上同时有 `recv` 和 `send` 的类型，
没法放进同一个 `tokio::select!` —— 两个 future 会同时可变借用它。所以按 `protocol` 里
sink/stream 的老办法拆开：

- `Inbox` 拥有网关 socket：鉴权、心跳、断线重连、把事件解码成 `Incoming`。
- `Outbox` 只是 HTTP API，`&self` 就能发，桥接循环里随时可以回话。

`channel::qq` 是第一个实现。加第二个平台（飞书长连接、钉钉 Stream）只需要再写一个
`Inbox`/`Outbox`，`main.rs` 和 `chat.rs` 不用动。

### 一个会话怎么对上

`chat → session` 的映射是**算出来的，不是存下来的**：`qq-g-<group_openid>` /
`qq-d-<openid>`。平台 id 本来就唯一，所以名字唯一；没有映射表可丢，重启后每个群接着原来的
transcript 说，不会开新会话。

一次消息的完整往返（全在 `main.rs` 的状态机里）：

```text
收消息 → 去重（MsgId）→ 白名单 → 不知道游标就先 get_messages(limit=1) 问 last_seq
      → set_session(幂等) → send_message → 立刻回「收到，正在跑…」(msg_seq=1)
      → Status{Idle} → get_messages(after_seq=last_seq) → 取最后一条 assistant text
      → 回结果 (msg_seq=2)
```

**回复从 transcript 读，不从 `response` 增量拼。** 增量是给屏幕看的：reasoning、
tool-call 参数、output 三段交错，一个 turn 里会被工具调用切成好几段，拼出来的「最后一句」
取决于你用什么规则切。transcript 里的行本来就是 typed 的（`kind`/`role`），「最后一条
assistant text」没有歧义 —— 这也正是 `get_messages` 存在的理由。

游标（`last_seq`）是桥接器**唯一的隐藏状态**，且只在这个进程里。第一次说话前必须先问一次
「transcript 现在到哪了」，否则回读窗口会从 0 开始，把会话的全部历史拉回来、把某条旧回答
当新回答发出去。

### QQ 平台的硬限制（官方文档）

| 限制 | 后果 | 这里的做法 |
|---|---|---|
| 被动回复只对 **5 分钟**内的消息有效，一条消息最多回 **5 次**（`msg_seq` 1~5） | gongga 一个 turn 可能跑十分钟 | 先回 ack（seq 1），结果用 seq 2；passive 被拒就降级成主动消息 |
| 主动消息频控：认证 60/min、未认证 30/min，单群 20/min、每天 1000 条 | 一个人用远够，公开服务不够 | 不额外做限流，把失败写进日志 |
| 群消息**不支持流式** | 无法把 delta 边跑边发 | 回复必须是拼好的整段，超长按字符切（`max_chars`，默认 1800） |
| 新机器人默认开 **IP 白名单**，只作用于正式环境 | 上线要有固定公网 IP | 默认 `sandbox = true`：沙箱不要审核、不要上线、不受 IP 白名单约束 |
| 平台会重投没及时应答的消息 | 一条消息变三个 turn | `MsgId` LRU 去重，窗口 5 分钟 |

### 安全：拿到 bot 的人等于拿到 shell

这是整套设计里唯一不能出错的地方。agent 有一把真的 Git Bash、零审批，所以：

- `[bridge] allowed` **白名单，空 = 谁都不能用**（失败关闭）。第一次被拒时把对方的 id
  写进回复和日志 —— 那是运维唯一能拿到自己 id 的地方。
- IM 侧**不暴露 `cancel`**（它会停掉整个服务端，连带别的会话）。要停就发 `/stop`，
  桥接器把它翻译成 `interrupt_session --session <会话>`，只停那一个 turn，并且把还在等答复的
  队列清空 —— 否则下一个 Idle 会去回读、把用户刚撤回的问题的答案发出来。
  只认带斜杠的形式：`cancel the last commit` 是要 agent 干的活，不是要停 agent。
- 用户文本**永远只作为 `send_message` 的 `message` 参数**，绝不拼进命令行。整行输入只有
  「命令」一种语法（§5），本来就挡住了注入，但桥接器一旦图省事写成 `sh -c "gongga $text"`，
  前面所有的努力都会作废。
- 更彻底的做法是给 bot **单独的 home + 端口 + key**：`server.lock` 保证一个 home 只跑一个
  服务端，于是 IM 那套 session、transcript、允许执行的目录天然和你的交互式会话隔开。

### 已知没做的

- **飞书 / 钉钉**：两者都免公网（长连接 / Stream），体验比 QQ 好（飞书没有 5 分钟被动窗口），
  但事件是 protobuf 且官方 SDK 只有 Go/Python/Node/Java —— Rust 要么手写帧协议，要么挂个
  sidecar。留给下一个人。
- **流式进聊天窗**：QQ 群消息不支持，飞书卡片可以，等接飞书再说。
- **断线续传（op 6/7/9 的 resume）**：现在断了就重新 identify，代价是断线期间的事件丢掉。
- **群聊里多人同时说话**：`waiting` 是 FIFO，答案按 turn 顺序回给对应的人，但同群两个人
  同时发问会各自触发一个 turn（服务端排队），回复是「一句一答」而不是「按人合并」。
  【待确认】要不要在群场景下加「一次只跑一个 turn，其余回排队提示」。
- **【待确认】** 去重表只在内存里：桥接器在 5 分钟窗口内重启，同一条消息可能被再跑一遍。
  落盘要再开一份存储，而错的代价是「多跑一次」而不是「丢活」，暂时接受。
- **【待确认】** `on_idle` 拿到 Idle 就去回读，如果这期间用户又发了一条，回读窗口会把新的
  用户消息也包含进去 —— `final_reply` 只取最后一条 assistant text，所以不会串话，但那条
  新消息的回答会留到下一个 Idle 才发。要不要用 `after_seq = 发消息前的 last_seq` 锁死窗口？

## 10. 自我进化（agent 换掉正在跑它的那个服务端）

目标：agent 改了 `crates/server` 之后，能让**正在跑自己的那个进程**退场，由新构建接上，
并且这个回合不丢。三件事拼起来就是通道，它们都是既有的：

| 部件 | 位置 | 在进化里扮演什么 |
|---|---|---|
| `cancel` 留活 | `commands.rs::cancel` → `cancel_and_preserve()` | 停服但把本回合留在 `pending_tasks` |
| 启动时拾起 | `commands.rs::resume_pending` | 新服务端起 bind 后把该回合接着跑 |
| `bash --detached` | `tools/detached.rs` | 继任者必须活在旧进程之外，否则一起死 |
| `server.lock` | `instance.rs` | home 是**交接**不是抢占；旧 pid 还在就拒绝启动 |
| `gongga`（在 `PATH` 首位） | `cli::server::write_shim` | agent 从 bash 里给自己发 `cancel` |

顺序是有原因的，`.agents/skills/evolve/evolve.sh` 把它固化成两条命令：

```
.agents/skills/evolve/evolve.sh stage          # 测试 → 构建 → 复制 → 写 launcher（不重启任何东西）
bash --detached: <launcher>      # 继任者在旧进程之外等端口
.agents/skills/evolve/evolve.sh swap <stamp>   # 确认继任者真的在等，然后 cancel
```

**为什么先起继任者再 cancel**：反过来（先 cancel、再起新进程）一旦新二进制起不来，
home 就空着，没有进程去 resume 这个回合 ���— 一次失败的进化会吃掉用户的活。现在失败模式是
「继任者起不来 → swap 检查到 launcher 不在 → 拒绝 cancel → 旧服务端照常活着」。

**为什么 `stage` 用 `.dev/target` 而不是 `target/`**：Windows 锁住正在运行的 `.exe`，
而要被替换的那个服务端常常就是 `target\debug\gongga.exe` —— 在 `target/` 里构建会
以 `拒绝访问 (os error 5)` 失败，恰好卡在「想自我进化」这个动作上。私有 target 目录让
构建与 `target/` 的占用状态无关，也让 `dev.sh` 那套「用副本跑」的纪律继续成立。

**launcher 的两个条件缺一不可**：旧 pid 消失 **且** 端口空出来。少了后者，新进程会抢在
旧 socket 释放前 bind 失败；少了前者，`server.lock` 会认出旧 pid 还活着而拒绝启动（这是
`instance.rs` 的正确行为，不该为进化放宽）。

**只有最新的 stage 能接管 home**：`stage` 把 stamp 写进 `.dev/evolve-current`，launcher 每轮
读它，发现自己不是当前的就在循环里退出；`swap` 也拒绝非当前的 stamp。这不是预算（等待仍然
没有上限，仍然是「等到它来」），是身份检查。没有它的时候，一次没被 `swap` 的 `stage` 会留下
一个**永远在等**的继任者：每 200ms 一次 `tasklist` + `netstat`，旧服务端不死就永不退出；它
已经被记进 `detached_jobs`，于是每个后来的服务端都收养它一次，`list_bash` 里永久挂着一条
杀不掉的作业；更糟的是端口空出来那一刻，它可能比真正的继任者先到，把更旧的二进制顶上去。

**`clean` 是这条规则的兜底**：早于这条规则的 launcher 没有自退出口，只能靠
`evolve.sh clean` 杀掉。`--artifacts` 顺手清掉旧 stamp 的暂存件 —— 每个 `-server.exe` 约
18MB，而每个 stamp 留下的三件套没有任何代码会删（一个 stamp 三件，`stage` 只增不减）。

**launcher 生成必须用带引号的模板**：原来的 unquoted heredoc 会让模板里的反引号在**生成时**
当命令替换执行。真实事故：注释里提到 `tasklist`，`stage` 就把整张进程表注进了 launcher，
继任者随即死在第一行。现在模板是 `<<'LAUNCH'`，值由 `sed` 从占位符换成实参，并在写完后再
`bash -n` 一次。

验证过的部分：`stage` 在本机跑通（含 162 个测试）；launcher 的两个分支分别用「活 pid」
「死 pid」+ 独立端口验过（该等的时候等，该死的时候 exec）；新二进制在独立 home/端口上
真的接得住客户端（`set_session` / `get_messages` 回执正常）。身份检查另在沙箱里验过四条：
被取代的 launcher 退出而不 exec、当前的 launcher 照常接管、`swap` 拒绝旧 stamp、`clean`
杀掉陈旧等待者并保留当前 stamp 的文件。

### 顺带修掉的两处

1. **`conns` 只增不减**（§8.19）：`serve` 给每个 peer 留一条 `Sender`，只在「往旧地址发包
   失败」时才删，而客户端重连后换了本地端口，旧地址再也不会来包。现在 `handle_connection`
   结束时自己释放那一项，并用 `conn_id` 保护 —— 同一地址已被新连接占用时，旧任务不得删掉
   新 peer 的条目。
2. **`GONGGA_CLI` 可能是条敲不通的命令**：当初服务端只找「本进程的同级目录」，而 `dev.sh server`
   只把服务端复制进 `.dev/`，同级就没有 `gongga`，于是回退成裸名 `gongga.exe` —— 不在 `PATH` 上，
   agent 拿到的是一条必死的命令。当时改成「同级 → `../target/debug/` → `PATH`，都没有才 warn」。
   这一条后来连责任一起搬走了：写 `<home>/bin/gongga` 的是 CLI 自己（它才知道自己在哪），服务端只把
   那个目录排到 `PATH` 首位。理由与两个坑（原生 `PATH` 被 Git Bash 改写、`cmd` 这个名字已经是
   Windows 自己的 shell）见 `docs/notes/server-shell-command.md`，起服务端那一段见 §5「谁起服务端」。

### 仍然存在的风险（**未做**，需要你定）

1. **杀自己的那条路已经堵上，但只堵了「祖先」这一种形状**：launcher 用 `exec` 时，行里的
   pid 就是新服务端自己，`adopt()` 丢弃它；launcher 改成 `spawn` 加等待时，行里记的是服务端
   的**父进程**，`taskkill /T` 会把服务端连根拔掉。现在 `adopt()` 拒绝收养任何「自己或自己的
   祖先」，`stop()` 落刀前再挡一次。剩下的缺口是 **pid 复用**：行里的 pid 只验活性、不验
   「是不是当年那个进程」，被回收的 pid 仍可能被当成作业收养，代价由显式的 `del_bash` 兜底。
2. **没有回滚**：新二进制起来了但行为是坏的（比如 LLM 配置读错），`swap` 无从知道。
   要不要让 `swap` 等新服务端 hello 之后再删旧 launcher，或保留「上一版 exe」一个指针，
   让下一回合的 agent 能一键退回？
3. **`swap` 之后本回合的叙事会被打断**：`cancel` 让当前 turn 以 `Cancelled` 收尾，新服务端
   是**再发一次请求继续**，不是无缝续写。用户会看到一段输出缺口，且这一次续写要重新付一次
   上下文费用。要不要把「进化」做成只在两个 turn 之间发生（比如排队到 idle 再换）？
