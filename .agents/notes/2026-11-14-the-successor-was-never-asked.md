# swap 问了「有没有人在等」，从不问「它起不起得来」

2026-11-14 00:50，一次 `evolve.sh swap` 把 home 上的服务端 cancel 掉，继任者 6 秒后起来、
读了 3 行 config、报 `unknown field tunneled`、退出。home 空着，TUI 里那个回合被 cancel 之后
没人接。用户问的是「谁关了我的 server」，真正的答案是「取消本身没做错，错的是它取消给了一个
读不懂这个 home 的进程」。

## 断在哪

`swap` 的四道守卫（stamp 是 current、launcher pid 活着、心跳 10s 内、outside claim 存在、
老服务端还在）全部通过，因为它们回答的是同一类问题：**有没有人在等**。没有一条回答
**它能不能跑**。

而「能不能跑」在那次事故里根本不是代码错误，是**数据撞 schema**：

- `e393449f`（00:26）把 `tunneled` 从 `[tool.*]` 里删了，`4823711a` 那批又把
  `timeout_ms` / `carrier` / `owns_its_wait` 删了；
- 活着的旧服务端是 `5b4784bd-dirty`，它不认这些删除，启动时照样把自己的字段写回
  `~/.gongga/config.toml`；
- 继任者是 `8ae65132-dirty`，`deny_unknown_fields`，第一次读这份 config 就拒绝。

`stage` 只跑 `cargo check --workspace --all-targets` 和 `cargo test`——它验的是代码，
从不读 `~/.gongga/config.toml`。而且顺序上也没得救：config 只在 `exec "$NEW" server` 之后
才解析，那时旧服务端已经在 6 秒前被 cancel 掉了。

## 补的那道门

`gongga server --check-config`：用一次启动的方式读 config，打印读的是哪个文件，退出 0；
读不了就是 `Error:` + 退出 2。它停在 home lock / db / ship install / bind 之前，
所以拿它去打一个活着的 home 不碰任何东西。

`evolve.sh swap` 在 `disk_line` 之前、cancel 之前调它，用的是**继任者自己的二进制**和
**这个 home 当下的 config**：

- 继任者自己的：认不认字段是二进制决定的，脚本读不懂 schema；抄一份 config 到临时 home
  去试启会被 ship / 路径之类的无关原因误判成「起不来」。
- 当下的：活着的服务端会重写 config，`stage` 那一刻的内容不是 swap 这一刻的内容。

拒绝就 `die`，旧服务端继续握着 home，回合继续跑——拒绝不花钱，cancel 落到一个起不来的
继任者身上才花钱。

## 两个坑

1. **`$( ... )` 会卡死**。读 config 要读 provider roster，读 roster 会 spawn 发现模型的
   脚本，脚本继承 stdout；命令替换等的是那个句柄，不是进程退出。于是 `check="$(...)"`
   永远不返回。改成写文件、读文件、看退出码。
2. **老 bundle 不认这个 flag**。`.dev/evolve-20261114-004451-gongga.exe` 是被取消的那次
   交接留下的，clap 报 `unexpected argument '--check-config'` 退出 2。门认这句话，单独
   回一句「这个 bundle 比 flag 老，重新 stage」，而不是谎称「读不懂 config」。

## 这次事故留下的 home 状态

config 里 13 行过期字段被删掉（旧构建写进去、新构建不认的）：
`tunneled`×6、`carrier`×4、`timeout_ms`×2、`owns_its_wait`×1。删法是机械的：`--check-config`
每次只报第一个未知字段，删那一行、再跑，直到退出 0。

恢复用的是新构建（`707bb371-dirty`），从一条活着的 channel 递进去起——不在本回合的 job 里，
所以它活得过这个 session。`--parent 56944` 是那扇还开着的 TUI，服务端的寿命仍旧绑在它上面。
起来之后它把被 cancel 的 `session=fix` 接着跑了（`resuming turns left pending`），这正是
evolve 设计里「继任者 resume 本回合」的那一步，只是晚到了半小时。

**这条 channel 只撑了 28 秒。** 服务端 17:21:01 起来，17:21:29 无声消失——日志断在一行
`channel started` 中间，没有 `server stopped`，也没有 `main: dropping runtime`，是被
`TerminateProcess` 掉的。那条 channel（`ch-85ba40d2…`）属于**上一个**已死的服务端：新服务端
启动时把它当遗留 job adopt 了（`channels the last run kept, reported gone adopted_channels=1`），
随后关掉它时，job 把它的子进程一起收了——而这个服务端正是那条 channel 的 bash 起的。
教训：**别把 home 的服务端递给这个 home 遗留的 channel**。服务端的寿命本来就该由窗口决定，
所以恢复它的正路是重开那个 TUI 窗口（`--parent` 那一路），不是从别的进程树里硬塞一个。

`server.lock` 里留下的死 pid 不用手动删：`HomeLock::acquire` 认「pid 不在了 = 垃圾」，
会自己接手。

我没有动旧构建的 `~/.gongga/bin/*`：它本来就被这次失败留在原地，新构建启动时自己装的。
