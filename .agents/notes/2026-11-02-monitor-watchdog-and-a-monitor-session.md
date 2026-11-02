# Monitor：看住进程的 watchdog，看住会话失败的一个 monitor 会话

User：「根据SPEC设计 monitor 方案」「写计划文档，然后实现」

本文件是计划与决策记录；实现完成后，`DESIGN.md` 与 `docs/notes/` 记录结论，这里留下
取舍的理由。

## 0. SPEC 的两句话是两个机制

SPEC §Monitor 说明两件事，失败层级不同，混在一起做会互相拖累：

| SPEC 原话 | 崩塌的是 | 机制 |
| --- | --- | --- |
| `server is wrappered by a watchdog process that auto restart if it crashed` | **进程**（panic、abort、被 kill） | supervisor 进程，重启子进程 |
| `once a session stopped with error, auto spawn a moniter session told to fix` | **会话**（turn 以 `error` 收尾：403/429、供应商挂了、工具把 turn 弄崩） | 服务端读自己的 store，把一个**普通会话**叫 `monitor`，把报告交给它 |
| `moniter session will be choosen from usable flash, auto switch if it also errored` | — | 从 roster 里挑 `tier = "flash"` 且有 credential 的模型；它自己失败就换下一个 |

同时 SPEC 的 diff 删掉了 Polity 的 `HR (flash)` 一行：看住别的工作、修 quota 与 harness
故障，从「一个 agent 扮演角色」变成「服务端自己的机制」。于是 monitor 会话**不是**角色，
它是一个普通会话，只是名字保留。

`~/.agents/skills/polity/SKILL.md`（不在本仓库）里 HR 仍然存在，需要用户另行退役；
本仓库只改 `ship/skills/`。

## 1. watchdog：`gongga-server` 二进制自己就是 supervisor

### 1.1 为什么在 server 二进制里，而不是新 crate / cli / bundle

三个入口都直接跑服务端二进制：`cli::server::start()`（窗口、`ensure`）、`bundle server`
（payload 里那个）、**以及 evolve 的继任者**（launcher 从旧服务端 argv 抄一份然后
`exec "$NEW" server ...`）。只有「自我监督是服务端二进制的固有行为」这一种形状同时覆盖
三者——换手之后仍然被监督。

```
gongga-server …          <- supervisor：不建 runtime、不拿 server.lock、不懂协议
  └─ gongga-server …     <- 子进程：GONGGA_SUPERVISED=1，真正的服务端
```

- 开关用**环境变量**，不用 argv：evolve 抄 argv 不抄 env，继任者于是自动重新被监督，
  而它抄到的 argv 仍然诚实。
- 不新增二进制、不动 `bin/` 的两个名字、不动 bundle 的 payload。
- `--version` / `--help` 在分叉之前回答，位置不变。

### 1.2 argv 原样交给子进程，`--parent` 不动

子进程拿到同一份 argv，`--parent <窗口 pid>` 照旧。于是：

- 窗口消失 → 子进程走现有 `signal::follows` 软停（turn 存成 pending）→ 退出 0 →
  supervisor 不重启。**supervisor 不需要懂协议、不需要发信号、不需要第二条生命周期。**
- `signal::adopt`（窗口认领无主服务端）导致的停止也是 0，同一条路。
- evolve 抄到的 `--parent` 仍指向窗口，不会指向一个马上要退出的 supervisor。

### 1.3 什么时候重启：退出码协议

今天所有结局都是 anyhow 的 0/1，supervisor 分不清「崩溃」和「起不来」。定一份最小契约：

| 码 | 含义 | supervisor |
| --- | --- | --- |
| 0 | 被叫停（cancel / 信号 / 窗口死 / adopt） | 不重启，以 0 退出 |
| 130 | 第二次坚持（现有 `asked()`） | 不重启，以 130 退出 |
| 2 | **说出了理由**（启动拒绝、迁移失败、prompt 缺项、`net::serve` 返回 Err） | 不重启，原因在日志里 |
| 其他（101 panic / 被 taskkill / 137） | 没来得及说 | **重启** |

改动只有一处：`main` 的 Err 路径从 anyhow 的 1 变成 `exit(2)`。「2 = 它自己讲了理由」
是唯一要长期维护的语义，它把「锁被活进程占着」这类确定性失败摘出重启循环。

（另一个候选是 `<home>/server.exit` 报告文件：能带长原因、不占用退出码，代价是多一个
home 状态文件，而且 supervisor 得为它再解析一次 home。选了退出码。）

### 1.4 退避

200ms 起、×2、上限 30s；子进程活过 60s 就复位。**不设次数上限**（AGENTS 禁 budget），
一个 panic 循环本来就该被无限重试，只是不烧 CPU。每个决定写一行 `watchdog: ...` 到同一个
日志文件，`server.log` 仍是唯一要读的地方。

### 1.5 supervisor 自己的细节

- 不建 tokio runtime：分叉发生在 clap 之前。
- 不拿 `server.lock`：锁归子进程，`server.lock` 里的 pid 仍是真正的服务端，evolve 的
  `running_pid` / `status` / 陈旧锁接管全部不变。
- stdio 继承：`server.log` 与控制台都不变，只是 supervisor 的行带 `watchdog:`。
- 忽略 Ctrl-C：`SetConsoleCtrlHandler(handler, 1)` / `SIG_IGN`，等子进程自己软停，
  然后把子进程的退出码交给 `serve` 的调用者。
- 子进程不 detach：它是 supervisor 的子进程，同控制台、同进程组。

### 1.6 两份必须跟着改的 pid 语义

「这个服务端是不是我起的」现在靠 pid 相等，多一层进程后：

1. `ServerInfo.supervisor: Option<u32>`，子进程从 `GONGGA_SUPERVISOR` 填。
   `tui::run::whose_server` 与 `stops_a_server` 接受 `spawned == pid || spawned == supervisor`。
2. `cli::server::stop` 的 kill 兜底：`cancel` 5 秒不应答时杀**整棵树**（Windows
   `taskkill /T`，Unix 杀进程组），否则刚被杀的服务端立刻被重新拉起，「停掉这个 home 的
   服务端」就不成立。

### 1.7 不动的部分

`server.lock` 的语义、`--parent`、`bin/` 的两个名字、bundle 的 payload、`serve` 的控制台
与退出码契约、evolve launcher 的等待与 supersede 守卫。

## 2. monitor 会话

### 2.1 触发：以 store 为真相，一次错误只交付一次

判据是持久的（不订阅 bus，广播会 `Lagged`，重启后也没有真相）：

```
sessions.status = 'error' AND sessions.monitor_seq < 该会话最后一条 message 的 seq
```

- 新列 `sessions.monitor_seq INTEGER NOT NULL DEFAULT 0`：上一个 turn 由
  `store.begin_turn` 先写入用户消息行，`last_seq` 因此严格增长，「这份 transcript 已经交
  出去过」用一列就够。
- 交付成功后写 `monitor_seq = last_seq`；交付失败不写，下一轮重试。
- 两个入口，同一个函数：启动时一次（跨重启的账）、已有 2 秒 tick 一次。
  加 `CREATE INDEX sessions_by_status ON sessions(status)`。

（另一个候选是在 `Agent::run_turn` 拿到 `TurnOutcome::Failed` 时立刻触发：更即时，代价是
改 `run_turn` 的返回形状，而且仍然需要启动扫描。选了扫描。）

### 2.2 一个 monitor 会话，不是每个错误一个

名字 `monitor`，`cwd` 与服务端相同（与新会话默认一致）。下发走 `commands::deliver`
同一条路：空闲就起 turn，忙就进 inbox 由下一步边界读入——排队门、`@skill:` 展开、shell、
工具、事件流、TUI 展示全部复用，没有第二套 spawn。`del_session monitor` 之后，下一个
episode 会重新创建它。

### 2.3 报告内容（服务端拼，不靠 skill）

会话名、`turn_end` 的 outcome/steps/elapsed、最后一条 `error()` 写进 transcript 的
`notice` 原文；build（`protocol::version_line()`）、home、pid、端口、cwd；
读回现场的确切命令 `gongga-cli get_messages --session <目标> --range -20:-1`；
`@skill: self-control` 引用（由 `expand_message` 展开成路径）；以及「诊断并修掉 harness
侧的原因」。同一 episode 只发一次；monitor 忙时它排在 inbox 里，不合并（合并要丢细节）。

### 2.4 选模型：usable flash

- `providers::Model` / `Entry` 增加 `tier: Option<String>`（roster 里 38 处 `tier = ...`
  今天被整体忽略，`docs/notes/server-providers.md` 也写着 carried through，这是补全）。
- 候选 = `catalog.models()`（credential 能解析）× 说 Responses × `tier == "flash"`。
- 排序：已知额度份额（最差的窗口）降序；未知额度按 roster 顺序排在后面（没报额度不是拒绝，
  与 `model_pool` 的 `fail_open` 一致）。`quota` 的内存状态从「最后一次 reading」扩成
  `provider -> Quota` 的表，`last()` 的语义保留给 `active_quota` 行。
- 选定直接写进 `sessions.model` 与 `Tuning`（走 `set_session` 那条 store 路径）。

### 2.5 monitor 自己也 errored：换下一个

`sections.monitor_attempts`（第二列，整数）：本轮 episode 里换过几次模型。monitor 的
status 不是 `error` 时清 0。轮到 monitor 自己 `status = 'error'` 时：

1. 取候选列表里 `attempts` 位置的那个（同时跳过等于当前模型的那一个）；
2. 写 `sessions.model` + `Tuning`，`attempts += 1`；
3. 把 monitor transcript 里最后一条 user 消息（就是那份报告）重投一次，前面加一句「上一次
   用的模型是这个，失败原因是这些」。

候选耗尽 → 一条 `Event::Log` + 日志，说明 monitor 停手；失败会话的 `error` 状态仍然可见。
同时把 `monitor_seq` 推到当前 `last_seq`，让这件事只发生一次（`monitor_seq` 的含义因此是
「这份 transcript 已经交给 monitor，或者已经放弃」）。不递归、不自造重试预算。

### 2.6 与 `model_pool` 的边界

服务端只读 roster 的 `tier` 与额度读数；`preference.toml` 权重、`mark`、entitlement 探测
继续留在 `model_pool` skill 里（那是机器的策略文件，不是 harness 的）。

### 2.7 服务端自己崩了也该有人修

supervisor 在「没来得及说」的重启里给子进程 `GONGGA_LAST_EXIT=<code>@<unix_ms>`；新服务端
启动时若看到它，就把这件事当作一份报告交给同一个 monitor 会话。零文件，只活一次——
正是需要的那一次。

## 3. 实现清单（每步一个提交）

1. `crates/server/src/supervise.rs` + `main.rs` 分叉 + `exit(2)` + 退避 + 忽略 Ctrl-C。
2. `protocol::ServerInfo.supervisor`；服务端从 `GONGGA_SUPERVISOR` 填；`tui::run` 两处判据；
   `cli::server::stop` 杀树。
3. `sessions.monitor_seq` / `monitor_attempts` 列 + 索引 + `store` 读写 + `migrate`。
4. `providers` 的 `tier`；`quota` 的 provider 表；`monitor::candidates`。
5. `monitor.rs`：报告拼装 + `sweep` + 换模型重投；`lib.rs` 启动与 2 秒 tick 各一次。
6. §2.7 的 `GONGGA_LAST_EXIT`。
7. 文档：`DESIGN.md` 两节、`docs/notes/server-supervise.md`、`docs/notes/server-monitor.md`、
   `docs/notes/README.md` 索引、`ship/skills` 里提到 HR 的段落。

## 4. 验证

- L1：`cargo test --workspace`（仓库现在没有测试，这一步是「没弄坏编译」）。
- `comments-check`、`test-layout-check`。
- L3（进程生命周期只有真进程看得见）：工作树自己的 `.dev/` 下建临时 home，内核分配端口，
  `kill -9` 子进程看是否被拉起、`cancel` 看是否不再拉起、窗口关闭看是否软停；
  跑完删掉临时 home。
- **不写测试文件**：AGENTS 写明 WRITING TEST IS NOT ALLOWED，用户未授权。

## 5. 风险

1. monitor 有完整 shell 权力：它会自己改代码、重启服务端。SPEC 说 told to fix，按这个做。
2. Windows 上 `taskkill /F` 杀 supervisor 会连子进程一起带走（`/T`），与「软停保 turn」
   相反；这是既有的「没人能替自己被谋杀做准备」。
3. pid 复用：`follows` 只验活性，`GONGGA_SUPERVISOR` 也只是个数字。
4. 重启与 `stop` 的竞赛：`cancel` 与 kill 之间的窗口仍然存在。
5. 额度读数只能在 step 结束时刷新，所以候选顺序可能是旧的；换模型时跳过「等于当前模型」
   那一项来兜底。

## 追加（同日，实现之后）：静态知识搬进 ship 的 `monitor` skill

实测一次 monitor 回合：112 秒、30 次工具调用，其中大部分在摸「一个 home 长什么样」。那部分
对每次失败都一样，所以按寿命切开：

| 放哪 | 是什么 | 代价 |
| --- | --- | --- |
| `@skill: monitor`（ship `b92d022`），进 prompt 的 index | 地图、分诊表（供应商/配置/harness）、只读 schema、安全规则、交付格式 | 付一次，本会话后续请求复用 |
| 消息本体 | 这次失败的会话/模型/行范围/build/home/pid/cwd、服务端写下的 `notice`、读回命令 | 付一次，且随 transcript 走 |

**为什么不放目录里的 `AGENTS.md`**：`meta::roots` 会把它当第一个根，但 monitor 的第一份 prompt
是在它知道自己的 shell 站哪之前建的（服务端创建的会话从服务端的 cwd 开始）。实测那次 index
恰好 12,017 字符＝只有 `self-control`，因为 cwd（`~/.gongga`）里没有 `AGENTS.md`；在那种目录
起服务端可复现。skill 走 `<home>/skills`，与 cwd 无关。

selftest 因此加了两条断言：`@skill:monitor` 必须出现在 prompt 行的 `rooted` 里，且报告必须是
「引用 + 这次的事实」。

顺手修掉的两个测试坑：`free_port` 原来挑的是 UDP 端口，而被 Windows 服务占着的是同名 TCP 端口
（实测撞上 `svchost`，fake 端点 bind 失败）；现在挑 TCP 并且**先拨号再往下走**。

### 更正：不是「服务端的 cwd」，是 shell 的 cwd 会动

第一版把原因写成「monitor 的第一份 prompt 在建的时候还不知道 shell 站哪」。**错了**——用户指出
「会话已经建立了，cwd 就是会话的 cwd」，这话对，数据也对：

- 那次失败**第一份** prompt（seq 2）的 index 是 21,400 字符，`@file:AGENTS.md` **在**；
- 丢的是后面的行。

真正的机制：`SessionHandle::cwd()` = `ShellSet::reported_cwd()`，即**会话 shell 的当前目录**，
`Shell` 每条命令后用 `pwd` 刷新；index 每步从这个活值重建，而 `prompt.cwd` 冻结在第一次存下的
值。于是行与行看起来同一个目录，背后却是 shell 当时所在。实测：seq 11 `cd /c/Users/17124/.gongga`
→ seq 13 剩 12,017（只有 `self-control`）；seq 45、63 `cd /c/resource/gongga` → seq 65 回到
21,400。

结论不变（skill 仍然是对的载体），但理由换了：skill 经 `<home>/skills` 解析，**shell 移动它也不动**，
而 monitor 整回合在 `cd`。顺带暴露一个真问题（DESIGN §11.3 第 6 条）：`cd` 一次就重写系统提示词，
每次都是一个 cache miss，与 `docs/notes/server-agent.md` 那句「只有真的编辑才重新计费」不符。
三条路（记录 cwd / 活 cwd / 折中）要用户定，我没动。
