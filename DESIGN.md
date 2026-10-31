脚本、模型、前端说的仍是同一句话。

这个键原来是 `Delete`/`Backspace`，换掉的原因是**一个键不能有两个意思**：行里没字的时候它删
会话，行里有字的时候它删一个字符 —— 到底发生哪件事取决于你打字打到哪儿了，而这两件事的代价
差着几个数量级。`Shift+D` 只有一个意思。它到达时是大写 `'D'`，所以判据是「这个字符 + Shift」，
不是「Shift + 一个小写字母」；行里已经有字时它照旧打出一个 `D`（大写字母还得能打），
所以只有**空的**输入行才真的删会话，没有 sticky 会话时它什么都不做。

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

**为什么不是 `gongga-cli set_session` 这种真子命令**：能做，但命令集是服务端握手时下发的，
本地没有固定列表可声明，所以走 clap 的 `external_subcommand` 兜住任何名字，再交给和
TUI 输入行**同一套** `submit()` 解析 —— 否则补全、sticky 补齐、报错要维护两份。
代价：`--server` / `--home` / `--key` 必须写在命令**之前**（external 参数会被整包吞掉），
所以检测到它们出现在命令后面时直接报错，而不是悄悄连到默认地址。

非交互模式**没有等待时限**：等的是一条命令的回执，不是它启动的活。服务端不回就会被自己的
`idle_timeout` 断连，那也是一个错误出口，不需要客户端再猜一个数字。

### 谁起服务端

SPEC 的结构是：`server` 和 `cli` 是两个**二进制**，`gongga-server` 由 `gongga-cli` 拉起来。于是「起一个
服务端」对使用者仍然只是一条客户端命令 `serve`：

```sh
gongga --home ~/.gongga serve --port 9700
```

它是**客户端命令**而不是隐藏开关：命令层本来就是「这个程序能被要求做什么」的清单，`serve` 因此
照常出现在目录里、能补全、能自我说明。它在 `cli::main` 里于任何 tokio 运行时建立**之前**被分派，
然后把控制台交给 `gongga-server`（Unix 上 `exec` 换掉进程映像；Windows 上继承三个 stdio 并等待，
把它的退出码带回来）：服务端要自己建它想被停掉的那个运行时，同一进程里嵌套一个会 panic。

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
刚才那个 turn 也就白跑了。`--home` 显式传给子进程：它要服务的是刚才放了那个命令的 home。

**不等它起来。** 窗口本来就在退避重连，再挂一个「等到 ready」的循环只是把同一个循环写两遍，错误
还不如它。起服务端要负责的是「把它拉起来」，不是「盯着它」。

`<home>/bin` 的那一对由**起服务端之前的那一步放**（`cli::server::install_commands`）：`gongga-cli` 是
起它的命令层程序，`gongga-server` 是被起的那一个，两个一起从同一份构建**链接**过去 —— Windows
硬链接，Unix 符号链接，跨卷才退化成拷贝。于是「谁起的这个服务端，它的 shell 和它的脚本就敲谁」，
而且只有一个名字：bash 自己会把 `gongga-cli` 补成 `gongga-cli.exe`，原生进程
按裸名字找到的也是同一个文件，`subprocess.run(["gongga", ...])` 不需要路径也不需要变量。

不搜索：同一台机器上可能有好几份构建，按 `PATH`、按兄弟目录、按 `bin` 里已有什么都可能挑到跟
这个服务端不是同一份的那个，而且没人会说一声。命令层找 `gongga-server` 同样只有三处写死的名字
（`GONGGA_SERVER_BIN`、本程序的同级目录、本 home 的 `bin`），不搜 `PATH`。硬链接不会跟着重建走（构建是换文件），所以每次
起来比较长度和 mtime，不一致就重放；符号链接不需要这一步。写走「先放旁边的临时文件、再 rename」，
因为 agent 可能正在跑那个名字 —— Windows 拒绝替换正在运行的 exe 时，旧的留到下次。

身份因此落在名字上（`tui/main.rs` 的 `named`）：以 `gongga-cli` 启动，裸调用就是目录；以 `gongga-tui`
启动，裸调用才是窗口。链接里塞不进 `cli` 这个词，而 `cli` 仍然可作为显式写法（`gongga-tui cli ...`）。

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

`[llm]` 这一段**基本不用写**：provider / base_url / key / 上下文都来自 `~/.agents/providers.toml`
（见 §4「名单」），这里只用来**覆盖**它。`GONGGA_PROVIDERS_CATALOG` 可以把名单指到别的路径。

库路径**不能**在启动时单独给：`--db` 与 `[server] db` 已删除，数据库恒为 `<home>/gongga.db`。

```sh
gongga --home /tmp/scratch serve --port 9700     # 另一个 home，整个状态都在它下面
```

一次性的运行 = 一次性的 **home**：库、日志、blob、作业记录、实例锁一起搬走。
它们以前可以分开搬，事故就是这么来的——锁按 home、库按 config，两者可以各说各话，
于是 scratch home 的服务端骑到了真库上，而没有任何一方看得见第二个写者。
（这里原先还写着 `GONGGA_DB=:memory:`，那个环境变量从来不存在。）

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
| 名单自动导入：`~/.agents/providers.toml` 15 个 provider，启动即用 | 通过（`provider roster imported ... providers=15`）|
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
| 模型从 bash 里调自己的 CLI（`gongga-cli cancel`，真实 provider） | 通过 |
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
| `Shift+D` 删 sticky 会话，`Delete`/`Backspace` 只编辑；空行 + 有 sticky 才删 | 通过（`shift_d_is_the_delete_key`；`Delete`/`Backspace`/裸 `d` 均不删）|
| 帮助行在窗口最后一行，写全每一个键位；窗口矮到放不下时先让出面板的行 | 通过（`the_help_line_is_the_last_row_and_names_every_binding`、`a_short_window_still_ends_on_the_help_row`，高度 3–7）|
| 补全：已填的 `--flag` 不再列出；Tab 完 flag 自动接上它的值列表 | 通过（catalog 单测 3 个 + tui 的 `apply_and_continue` 单测）|
| 长日志的一帧成本与日志长度无关（20k 行 156ns / 旧 8.1ms）| 通过（`examples/frame_cost`）+ 单测 `a_frame_touches_only_a_screenful` |
| 流式追加 = 整行重排（折行偏移一致）| 通过（单测 `appending_equals_rewrapping_from_the_top`，9 组 × 6 种宽度）|
| 数据库恒为 `<home>/gongga.db`：`--db`、`[server] db` 都已删除，scratch home 不可能骑到真库上 | 通过（230 个 lib 单测；真启动一次 scratch home，`server.lock` 与 `gongga.db` 同目录）|

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
5. **非交互输出只有 JSON**（见 §5）：人直接敲 `gongga-cli set_session --session x` 看到的是一行
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
    模型从 `bash` 里敲 `gongga-cli interrupt_session --session <自己>` 就能停；`cancel` 同理
    （`test1` 当年是用 `taskkill` 硬停的，现在没必要）。要不要**进一步**在工具层加一条豁免
    ——比如禁掉 `gongga-cli cancel`，免得 agent 一顺手把整个服务端关了？
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
    脚本、`gongga-cli interrupt_session --session x`（也就是模型自己）和 TUI 打的是同一条。
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
| `gongga-cli`（在 `PATH` 首位） | `cli::server::place` | agent 从 bash 里给自己发 `cancel` |

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
而要被替换的那个服务端常常就是 `target\debug\gongga-server.exe` —— 在 `target/` 里构建会
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
2. **agent 拿到的命令可能敲不通**：当初服务端只找「本进程的同级目录」，而 `dev.sh server`
   只把服务端复制进 `.dev/`，同级就没有 `gongga-cli`，于是回退成裸名 `gongga-cli.exe` —— 不在 `PATH` 上，
   agent 拿到的是一条必死的命令。后来改成「同级 → `../target/debug/` → `PATH`」，再后来这一条连
   责任一起搬走了：放进 `<home>/bin` 的是**起服务端的那个命令层程序**和它拉起来的服务端（两条链接），
   服务端只把那个目录排到 `PATH` 首位。理由与两个坑（原生 `PATH` 被 Git Bash 改写、`cmd` 这个名字
   已经是 Windows 自己的 shell）见 `docs/notes/server-shell-command.md`，起服务端那一段见 §5。

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
