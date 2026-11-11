---
name: evolve
description: 替换掉正在跑自己的那个服务端——改完 server 代码之后，让新二进制接管 home 并继续当前回合。用本 skill 目录下的 `evolve.sh`（stage / launch / swap / status / clean）。只在改过服务端代码后用它；改 skill 脚本、文档、TUI 不需要。换手会 cancel 本回合，靠 resume 接回来。
compatibility: 需要 bash 与 cargo，并在本仓库的 checkout 里运行
metadata:
  entry: evolve.sh
---

# 换掉自己

改完服务端代码，跑着的进程还是旧的二进制。这个 skill 把它换成新的，而且让**当前回合不死**。

```bash
SKILL_DIR=<本 SKILL.md 所在目录>      # `@skill(evolve)` 展开里的 `SKILL.md: <path>` 的父目录；gongga 里也可以是 `gongga-cli '{"name":"skill","args":{"name":"evolve","args":[…]}}'`
S="$SKILL_DIR/evolve.sh"

$S stage              # 编译检查 + 构建 + 暂存 + 生成 launcher（会先报磁盘余量）
$S stage --clean      # 同上，并让继任者在旧服务端退出后、绑定端口前跑 cargo clean
$S launch <stamp>     # 把 launcher 交给一条活着的 channel 去起（不在本回合的 job 里）
$S swap <stamp>       # 确认继任者真在等、且不是本回合孵出来的，再发 cancel
$S status             # 当前 stamp、谁在等、端口与锁、租约在谁手里；`successors:` 为空才算干净
$S clean              # 收掉没人会 swap 的旧继任者（--artifacts 连旧暂存件）
                      # 输出 "N killed, M left, K kept"：kept 那几行要说清为什么留
```

`stage` 先 fetch 内容仓、把 `local` rebase 到官方 `main`，再跑 `ship.sh --check`；
它也同步 `$GONGGA_HOME/ship`，因为继任者读的是 home 的 fork，不是源码树那个 checkout。
官方 `main` 必须是 home 当前内容分支的祖先。全部通过后才取 lease：preflight 失败的
构建不能占用 home，也不会等到新 server 启动时才发现缺少新工具文案。

## 规范：一次改动，一次换手

换手会 `cancel` 当前回合并让继任者重放它。所以**每一次 `stage` 都是一次真实的中断**，
不是"先试试"。下面几条不是建议，是踩出来的：

1. **一个改动只 `stage` 一次。** 你改完代码、跑完测试、`stage`、`launch`、`swap`——结束。
   想再验一个东西，改完再走一遍，**不要**在同一轮里连续 `stage` 三次。用户看到的是
   服务器反复死掉，且每次重放都把两个会话的上下文一起拉起来。
2. **并发这件事现在由脚本挡，不再靠记性。** `stage` 取一个租约（`.dev/evolve-lease/`，
   `mkdir` 原子），它一路活到继任者绑端口；期间第二个 `stage` 直接拒绝并说出持有者，
   `swap` 也只认租约上的那个 stamp。持有者都死了（stage 的 shell 没了、也没有 launcher）
   才算废弃，由下一个 `stage` 接管或 `clean` 释放。**被拒绝不是要你绕过，是要你先处理
   那一次换手**：`swap` 它，或者 `clean` 掉。
3. **`stage` 会写 `evolve-current`，并持有租约。** 已经 staged、还没 launch 的那一段也算
   持有——那是 `stage` 退出、操作者还没起继任者的窗口，正是最容易被第二个 agent 踩掉的
   时候。真正被取代（另一个 stage 把指针挪走）时，等待中的 launcher 自己退出并接管 home，
   所以**连续 stage 等于连续重启**，哪怕你一次 `swap` 都没发。
4. **验证脚本不许碰 9700。** 临时 home + 随机端口 + 跑完 `trap` 清理。碰了在跑的服务端，
   你测的就是"我有没有把自己的服务端弄死"，不是被测的东西。
5. **`swap` 之后不要马上再 `swap`。** 继任者在重放你的回合，写完一句话再决定下一步。
6. **`swap` 失败就停下看。** 拒绝的理由（stamp 不是 current、租约在别人手里、心跳过期、
   没有继任者在等）都指向一个具体问题；绕过守卫手动 `cancel` 只会让 home 空着。

一句话：**换手是交付动作，不是调试手段。** 调试用 `cargo test`、用临时 home 起一个
临时服务端，别拿正在跑你的那个当试验台。

## 为什么没有 `stage --launch`

试过，做不通，而失败的样子是"home 空着"——所以记在这里，别再试第二遍。

杀 shell 的是 `Shell::kill`：**有 job 时用 `TerminateJobObject`**，没有 job 才退回
`taskkill /T`。所以活过 `cancel` 的条件是「不在那个 job 里」，不是「不在进程树里」。

job 现在允许 `BREAKAWAY_OK`（`tools/job.rs`），也就是子进程**显式**传
`CREATE_BREAKAWAY_FROM_JOB` 就能离开；不传的照旧随 shell 一起收掉——`SILENT_BREAKAWAY_OK`
是另一个选项，但那会让每个子进程都离开 job，会话连自己起的进程都带不走，不行。

不显式传这个 flag 的启动方式我试了三种（`nohup`、`cmd start`、`Start-Process`）：它们在
命令结束之后都还活着，一到 `cancel` 就全死了，日志里没有一行优雅退出。

而**脚本里要传这个 flag，比看上去难**，实测踩到两个坑，任何一个都会让继任者起不来：

- 给子进程 `STARTF_USESTDHANDLES` 加三个空句柄 → `cmd`/`timeout` 这类程序**直接起不来**
  （空句柄不是 NUL 设备）。
- 让它继承句柄 → 它攥着 shell 的管道，于是工具调用一直等它退出：我量到一次 241 秒。

正确的做法是两件事同时成立：`CREATE_BREAKAWAY_FROM_JOB` + 子进程有自己的句柄（NUL 或文件）。
服务端的 `tools/detached.rs` 已经把这件事做对了（`Stdio::null()` + `kill_on_drop(false)`），
所以继任者**应当由服务端去 spawn**，也就是 `bash --detached`。

一个 shell one-liner 做不到这两件事同时正确，这就是为什么 `stage --launch` 不存在：
要让它成立，需要把「以 detached job 方式跑一个程序」开放给脚本——一条 server cmd，或者一个
专门的小程序。**那是要改 SPEC 的决定，不是这个脚本能自己解决的**，所以我没有在脚本里留一个
看起来能跑、实际会静默死掉的分支。

推论：`swap` 的守卫不是形式主义，它是唯一能挡住"取消了却没人接班"的东西。

## `launch` 走的是另一条路

`launch` 不自己 detach：它把 launcher 交给**一条活着的 channel** 的程序去起。relay 是
detached 起的（`tools/channel.rs` → `DetachedJob`），它的 bash 不在任何 job 里，所以这样起出来
的继任者天然活过 `cancel`。两个代价，都写在代码里：

- `swap` 必须用 `cancel --keep_alive true`。否则那条 channel 跟着服务端一起结束，继任者也一起没。
- 挑 channel 要**问过再挑**：程序丢了 stdin 的 relay 照样 `exit:null`，看起来和正常的一模一样。
  `launch` 会先让它写一个文件，两秒不答就换下一条（这台机器上 `ch-39e2c727…` 就是聋的）。

`launch` 会写 `evolve-<stamp>-launch.outside`，`swap` 只认这个声明：pid 活着、心跳新鲜这两件事，
在"能活过 cancel"和"马上要跟着 job 一起被收掉"两种情况下长得完全一样，从外面分不出来。手起的
继任者没有这个文件，`swap` 会拒绝并让你改走 `launch`。

2026-11-09 实测的失败形态就是这一种：继任者 17 秒心跳正常、`swap` 校验通过、`cancel` 之后
1 秒内消失，home 空着——它是本回合 shell 的子进程，`TerminateJobObject` 一并收掉了。

真要让 `stage --launch` 成立，得让服务端提供一个"以 detached job 方式跑一个程序"的命令
（脚本能调的那种），也就是改 SPEC 里 server cmd 的清单——那是另一个决定，不是这个脚本
能自己解决的。

## 为什么顺序不能反

`swap` 会 cancel 本回合。只有**继任者已经在等**，那个回合才有东西接：

```
stage → 起继任者 → swap(cancel) → 旧进程退出 → 继任者绑端口 → resume 本回合
```

反过来（先 cancel 再起），一旦新二进制起不来，home 就空着，没人 resume——回合真的丢了。
`swap` 的守卫就是为此存在（stamp 必须是 current + launcher pid 活着 + 心跳 10s 内 +
老服务端还在）。**不要绕过它手动 `cancel`。**

## 磁盘：为什么 clean 必须由继任者做

`cargo clean` 删的是 `target/`，而**旧服务端本体就是 `target/debug/gongga-server.exe`**。
Windows 锁住运行中的 exe，所以 `stage` 里 clean 一定失败（`os error 5`）。

正确时机只有一个：**旧进程已经退出、继任者还没绑定端口**那一段。也就是 launcher 的
等待循环结束之后、`exec` 之前 —— `swap --clean`（其实是 `stage --clean`）就是干这个的：

```bash
$S stage --clean     # launcher 里带上了 clean
$S status            # armed: the current successor runs 'cargo clean' before it binds
# 起 launcher，然后 swap
```

三条命令都报磁盘，因为**自我迭代的第一约束是空间**：

```
disk:  (free 31G of 476G, 94% used)
```

- `stage` 一开始报一次 —— 在构建需要空间**之前**知道；
- `swap` 报 `disk before:`；
- 继任者 clean 完报 `free X -> Y`；
- `status` 随时能看。

`cargo clean` 失败不会中断换手：树留着，继任者照常接管。空间不够是运气问题，接不上是
流程问题，后者严重得多。

## 换完怎么证明成功

不要相信自己写的笔记，也不要相信 `server.lock` 变了就算数。看**继任者自己的日志**：

```bash
sed 's/\x1b\[[0-9;]*m//g' ~/.gongga/detached/<最新>.log | grep -E "replaying a turn|resumed=true"
```

要看到这两行（时间戳应几乎相同）：

```
... commands: replaying a turn left pending session=<名> <你上一句原话>
... agent: turn start session=<名> resumed=true
```

**没有 `replaying a turn` = 没接上**，哪怕 `server.lock` 已经是新 pid、端口也绑着。

## 不要写恢复笔记

resume 是把**完整 transcript** 原样重放（消息、reasoning、每个工具调用和结果），外加
`handover`（如果培养过继承人）。"我打算干什么"已经在里面了，写一份笔记不会多提供任何信息。

而且它在两种真故障下毫无用处：

| 故障 | 笔记有用吗 |
|---|---|
| pending 行没留下 | **没用**——根本没人被唤醒，没人会读 |
| 已经培养过继承人 | 没用——该用 `heir` skill 的 `read` 去读 mentor 会话（原文只在 mentor 会话里，当前会话只有继承人的第一课） |

resume 之后要做的是**重新测量外部状态**，因为那部分 transcript 里没有、而且已经变了：

```bash
cat ~/.gongga/server.lock                    # 现在是哪个 pid
netstat -ano | grep :9700                    # 端口绑着吗
python -c "import sqlite3,os;c=sqlite3.connect(f'file:{os.path.expanduser(\"~/.gongga/gongga.db\")}?mode=ro',uri=True);print(c.execute('select id,kind from pending_tasks').fetchall())"
```

## 实测踩到的坑

**1. Windows 上 `exec` 不保留 pid —— 而且 launcher 会死。** launcher 是 bash，`exec` 一个
原生 `.exe` 之后：新进程拿到新 pid，bash 自己**退出**，只在 `server.lock` 那个进程的
`ParentProcessId` 里留下一个已经消失的 pid。（2026-11-02 实测；"bash 留下当父进程"的旧说法
是错的。）

由此两条：

- `server.lock` 的 pid ≠ `launch.pid` 里记的 pid，这是**正常**的，不是换手失败；
- 一个已经 exec 过的 launcher，`tasklist` 里根本查不到，所以 `clean` 永远选不到它 —— 但
  **手动 `taskkill /T` 选得到它上面的那棵**。换手期间不要杀任何 launcher 相关进程；要收
  就交给 `clean`，要停就 `swap` 完再停手。

MSYS 记的 `ParentProcessId` 不是可见 bash 进程的 Windows pid（三层链条实测报出的是已退出
进程的号），所以**不要**沿父进程链往上找"服务端在谁的子树里"：这条路上没有可靠答案。
`status`/`clean` 只断言两件查得实的事：pid 相等（unix `exec`），或它是服务端的直接父进程。

这还有一个后果：**启动 launcher 的那个 `bash --detached` 是换手树的一部分**。用
`del_bash` 收它时，harness 的祖先守卫会拒绝（它就挂在服务端的父链上）；绕过守卫去
`taskkill /T` 就是 2026-11-02 那次把服务端一起带走的原因。换手结束前不要收它。

**2. `.dev/target` 是旧设计留下的空壳，别再用它。** `$BUILD` 现在只剩一行定义：`stage`
和 launcher 都构建在默认的 `target/`，而继任者拿到的是 `cp` 出来的 bundle 副本，所以
"正在跑的 exe 锁住构建目录"不再是问题——被替换的从来不是 `target/debug/gongga-server.exe`。
这个目录只会一路长到十几 GB（本机量到 14 GB，磁盘 97%）；`rm -rf .dev/target` 不影响
任何一次换手。设了 `CARGO_TARGET_DIR` 时 stage 从 `$CARGO_TARGET_DIR/debug/gongga` 取
head，嵌套 payload 仍写死 `target/bundle-payload`（`build.rs` 的 invariant），不随之迁移。

**3. `stage` 构建整个发行版，不是一个程序。** bundle 把 server、cli、tui 装进一个文件，
换手一次就换掉三个程序；`--all` 是唯一模式（旧旗标只留一句 note）。构建产物是 `cp` 出去
的副本，所以用户正在用的 TUI / CLI 锁着 `gongga-tui.exe`、`gongga-cli.exe` 也不挡构建。

**4. 只有最新的 stage 能接管。** `stage` 写 `.dev/evolve-current`；被取代的 launcher 自己
退出等待循环，`swap` 拒绝非当前 stamp。重复 `stage` 安全，但**别指望用旧 stamp 回滚**
（它的继任者已经走了，要回滚就重新 `stage`）。

**5. 继任者会留在进程表里。** 每次 swap 多一个 `bash.exe`（服务器的父进程），随服务器
退出而死。不是泄漏。

**6. 反复 swap 会耗尽 MSYS 的 fork。** 这是实测踩到的、代价最高的一条：launcher 每
0.2 秒 spawn 两个进程（`tasklist` + `netstat`）看家是否释放，被放弃的等待者会永远轮询。
三个这样的等待者 = 每秒 30 次进程创建，于是**你自己的 shell** 开始报

```
*** fatal error in forked process - WFSO timed out after longjmp
child_copy: cygheap read copy failed ... Win32 error 299
*** fatal error in forked process - ccalloc would have returned NULL
```

那不是 gongga 的 bug，是 fork 失败。因此：

- launcher 现在**前 5 秒用 0.2s，之后退到 1s**（等待不紧急，不该每 200ms 付一次创建）；
- **swap 前后跑一次 `clean`**，别让等待者堆积（`clean --artifacts` 连旧暂存件一起收，
  实测 345M → 20M）。

**7. `cancel` 期间 waker 必须停手。** `cancel` 会**保留** pending 行（这是对的设计），
如果唤醒器在 shutdown 中把那一行捡起来，它会起一个新回合、把 `active` 钉在 1，
服务端就永远退不出去：

```
turn end outcome=Cancelled
woke sessions holding an undelivered message started=1
turn start resumed=true
still waiting for running turn(s) to stop active=1 waited=20s
```

`nudge_inboxes` 的第一行是 `if d.shutdown.is_cancelled() { return 0; }`，就是为了这个。
**如果进程退不掉、继任者一直等，先看日志里有没有这四行。**

**8. 先确认"谁在服务"再 swap。** `server.lock` 给出 pid，但它可能不是任何 evolve 二进制
（比如有人在 `target/debug` 直接跑了一个）。一次调用就能确认：

```bash
powershell -NoProfile -Command "(Get-CimInstance Win32_Process -Filter 'ProcessId=<pid>').ExecutablePath"
```

不是 `evolve-*-server.exe` 的话，你做的 swap 会一直"失败"，而且你会以为是脚本的问题——
我为此空转了三轮。

**9. 别在 `.dev` 上跑 `du -sh`。** 那里有 `.dev/target`（十几 GB 的构建产物），MSYS 上
扫一遍要几分钟，看起来像卡死。要看 evolve 占了多少，只统计暂存件：

```bash
du -ch .dev/evolve-*-server.exe | tail -1
```

## 开发环境：别用自己正在跑的那套

要验服务端改动（新端点、新工具、新 roster 字段），**不要用你自己正在跑的 home 和 roster**。
一套隔离环境只多三步：

```bash
# 1. 自己的 roster，指向自己的 fake 端点
cat > .dev/v/providers.toml <<'EOF'
[[provider]]
id = "fake"
  [[provider.endpoint]]
  api = "openai-responses"
  base_url = "http://127.0.0.1:8899/v1"
  ...
EOF

# 2. 自己的 home + 端口；roster 的入口是 GONGGA_PROVIDERS_CATALOG
GONGGA_PROVIDERS_CATALOG=$PWD/.dev/v/providers.toml GONGGA_KEY=sk-fake   ./.dev/target-test1/debug/gongga --home .dev/v/srv serve 8900

# 3. 客户端显式指过去，并且不要让它自己拉服务端
./target/debug/gongga --server 127.0.0.1:8900 --home .dev/v/srv --key sk-fake --client-only   set_session s fake/fake --cwd .
```

三个坑：

- **`HOME=` 不管用**：roster 走 `GONGGA_PROVIDERS_CATALOG`，`dirs::home_dir()` 在 Windows 上
  不读 `HOME`。设了 `HOME` 以为隔离了，实际还是读你真实的 `~/.agents/providers.toml`。
- **roster 是 `OnceLock`，进程启动读一次**。改了 roster 要重启服务端才生效。
- 用 `scripts/fake_llm.py` 当端点，`FAKE_LLM_DUMP=<path>` 会把**真实请求体**逐行写下来——
  想看"服务端到底发了什么"，这是唯一可信的办法，别靠读代码推测。

日常换二进制用 `scripts/dev.sh`（把 exe 复制出 `target/` 再起，避开 Windows 的文件锁）。
验完收尾：关掉测试服务端（否则 `target/` 里的 exe 锁着），删掉 `.dev/v/`。

**别信"某条测试会失败"这类断言——先自己跑一遍再转述，包括本文档里的。** 踩过一次：
有记录说私有 target 目录会让 `cli_path` 测试失败，实测 `CARGO_TARGET_DIR=.dev/target-test1`
下那条测试是通过的（`cli_path()` 有三层兜底）。

## 症状：代码改了，命令目录还是旧的

最常见的「我以为改完了」不是构建失败，而是**跑着的服务端还是旧 build**。特征：

```bash
$ gongga-cli                        # 命令目录里还是旧的参数名
12 command(s); run one with `gongga <command> <args>`; each takes line says its shape.
set_session -- takes: --name <name> --model <model> [--thinking <level>] [--ctx_limit <n%>] ...
                                                   # 而源码里早就没有 --llm 了
```

命令目录是编进二进制、由服务端发布的，所以**源码正确不影响它**。确认到底是谁在服务：

```bash
cat ~/.gongga/server.lock                                    # pid
powershell -NoProfile -Command "(Get-CimInstance Win32_Process -Filter 'ProcessId=<pid>').CommandLine"
```

`ExecutablePath` 不在 `target/` 也不在 `payload/<本 build>/` 下，就说明是别人的继任者或旧暂存件
——它不会自己更新，只能走一次本 skill。**别去改源码找原因**，源码多半是对的。

顺带一条：修完 `store.rs` 这类 schema 迁移之后，旧服务端的进程仍然持有旧代码，即使数据库
已经被新二进制迁过了——两边对同一个库的假设不一样，这就是「什么都对不上」的来源。

## 一次换手换的是整套发行版

`stage` 构建的是**bundle**（一个文件携带 server / cli / tui），继任者也**就是**它：

```
stage:  cargo test → cargo build -p gongga-bundle → 暂存 .dev/evolve-<stamp>-gongga.exe
launch: 等旧服务端退出
exec:   GONGGA_HOME=<home> <bundle> server <旧服务端的 argv>
        → bundle 把三个程序装进 <home>/payload/<stamp>/
        → 退休别的 payload 目录（正在被窗口用的那个留着）
        → 用它刚装的 gongga-server.exe 服务同一端口、同一 home、同一 owner
```

所以换手之后：`payload/` 是新的三个程序、`bin/gongga-cli.exe` 与 `bin/gongga-server.exe`
是新的一对（服务端启动时按自己的构建刷新它们）、跑着的服务端也是新的。**唯一没换的是正在
运行的那个窗口**：`gongga-tui.exe` 无法替换自己，重开窗口才会用上新的 tui；但它的 payload
目录不会被删，所以旧窗口照常工作到你自己关掉它。

`stage --all` 因此不再需要（所有程序本来就都跟着走），传了也只会打一行提示。

## 继任者带着旧服务端的命令行

SPEC（Skill: evolve）：*restart server should still keep its lifetime, e.g. tui
spawned server's lifetime bound to tui*。所以继任者不是「一个同名新进程」，而是
**同一个服务端换了个二进制**：`stage` 在旧服务端还活着时把它自己的 argv 读出来，
一个 token 一行写进 `$RUN/evolve-<stamp>-args`，launcher 原样 `exec "$NEW" "${ARGS[@]}"`。

```bash
$S stage
old server: home C:\Users\x\.gongga, bind 127.0.0.1:9700, owner 8736
# 继任者起来时：
evolve: taking the home as C:\Users\x\.gongga on 127.0.0.1:9700 for window 8736 (6 argument(s) carried over)
```

四条由此而来的规矩：

- **`--parent` 必须跟着走。** 丢它的继任者是「无主服务端」：关窗口不会停它，它会一直占着
  端口和 home 锁，直到有人手动收。
- **`--home` / `--bind` 也必须跟着走。** 它们只是**恰好**等于默认值时才看不出区别；换到
  第二个 home 或非默认端口时，丢了就等于把服务端挪了家。
- **目录跟着走，而且不是 home。** 服务端继承启动者的目录，会话的默认 cwd 就是它；继任者
  落在 home 里，之后每次不带 `--cwd` 的新会话都开在 `~/.gongga`。旧服务端在 claim home 时
  记下了自己跑在哪（`<home>/server.cwd`），`stage` 读它、launcher 在 `exec` 前 `cd` 过去。
  没有这条记录（早于它的服务端）时落在仓库根，并在 `stage` 的输出里说明。
- **读不到旧服务端的命令行 = 拒绝 stage。** 猜一个 argv 正是产出无主服务端的那条路。
- **窗口在继任者接管前就没了 = 拒绝接管。** 带一个已死的 `--parent` 起来，它会 resume 你
  的回合再立刻自杀；那时该做的是重开窗口。

## 症状：TUI 关了，服务端还在

说明那个服务端不是窗口起的（`--parent` 只管窗口自己 spawn 的），而是从别处来的——最常见
就是被放弃的 evolve 继任者。窗口连上它会**认领**它（`signal::adopt`，见
`docs/notes/server-signal.md`），此后关窗口就会停掉它；但**认领发生在连上之后**，所以
「窗口已经关了」这种情况下只能手动收：

```bash
$S status            # 谁在等、端口与锁
$S clean             # 收掉没人会 swap 的旧继任者（--artifacts 连旧暂存件）
```

## 什么时候不需要它

- 只改了 skill 脚本 / 文档 → 不用换（它们不在任何二进制里）。
- 改了 TUI → 换手会把新 tui 装进 `payload/`，但正在运行的窗口是旧的；关掉重开窗口即可，
  不必为了 tui 换服务端。

## 代价

`swap` 之后本回合被 cancel 并保留，继任者重放它。残余风险见 `DESIGN.md` §10。
