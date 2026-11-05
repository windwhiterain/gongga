# 两种 monitor，和一台设备"还剩多少空间"

日期：2026-11-05 · worktree `.worktrees/monitor-low-disk` · 分支 `feature/monitor-low-disk`

## 用户要的

> 现在只有 session error monitor，新增一种 monitor 来清理磁盘，已经有对应的 skill 了，
> 不同的 monitor 用 `@monitor/xxx` 名字。

> 整个 device 共享一个（安全文件），多个 server 如果同时触发，如果发现贡献的安全文件被删，
> 就抑制这次触发。所以说安全文件的查看+删除能合并为原子操作吗

然后**第一版被实测否掉**：

> 实测不行，磁盘低了跟本触发不了 tool error，删掉安全文件，改为每个 tool call end 都检查，
> 低于配置的阈值就触发

## 用户定的（第二版，本文照它实现）

1. 没有安全文件、没有预留、没有 claim、没有重新上膛。
2. **每个 tool call 结束都读一次剩余空间**（成功也算），与 `[server] disk_low_mb`（默认
   1024 MiB，`0` 关闭）比较。
3. 低于阈值就触发；一次"落下"只报一次，回到阈值以上再落下才算新的一次。

## 第一版为什么没被唤醒（实测记录）

用户测出"磁盘低了根本触发不了 tool error"。我先怀疑过文本匹配、再怀疑过预留文件被别的
server 花掉，最后能量到的事实是这几条：

| 事实 | 怎么量到的 |
| --- | --- |
| 活着的 home 里从来没有磁盘事件 | `~/.gongga/gongga.db`：`kind='disk_space'` 0 行，会话只有旧的 `@monitor` |
| 报错分支一次都没跑过 | `~/.gongga/server.log`：`spent the device's disk reserve` 0 条，`could not spend the disk reserve` 0 条 |
| 活着的服务端就是第一版的二进制 | 二进制里能搜到 `disk reserve is armed` / `spent the device`，搜不到第二版的 `the room left is read after every tool call` |
| 之后这台机器真的把盘写满 | `cargo` 报 `磁盘空间不足。 (os error 112)`；`(Get-PSDrive C).Free` 读到 0；删掉主 checkout 的 8.2 GB `target/debug/incremental` 才恢复 |

结论是设计本身的问题，不是某一次的巧合：**一个"只在工具失败时看盘"的判据，遇上一台主要
在读的机器，等于看不见**。磁盘从"有点紧"到"写不进去"之间，`cat`、`ls`、`grep`、只读的
shell 全都正常。

## 一次悬而未决的怪事（留给下一个动手的人）

把磁盘那一支写进 `monitor_selftest.sh`（在 error 那一支之后）时，它**从来不会触发**；同样的
三步（同样的 home 形状、同样的两份 roster、同样在 home 的 `config.toml` 里写阈值）放进
`disk_selftest.sh` 就有事件。量到的现象是：

- 服务端日志有 `command ran command=bash session="spill"`，CLI 也拿到了 `hello`；
- 但我在 `tool_cmd::run` 和 `disk::note_tool_end` 各插了一行带 pid 的 debug，
  **两行都没有出现**（而 `disk::install` 那行出现了，pid 相同、阈值也读对了）。

按 `commands::dispatch` 的写法，`bash` 只可能走 `tool_cmd::dispatch_tool`，所以"命令跑了但
两个函数都没进"在当前代码里是解释不通的——要么当时答这条命令的不是那一个进程，要么还有第三
条路径没被看到。没有定论，所以**把磁盘那一支拆成自己的脚本**（`scripts/disk_selftest.sh`），
两边都绿，并且把这个现象原样记在这里，而不是把"绿"当成答案。

## 一个测试上的坑（记下来，省下一个人的半天）

selftest 的临时 home 里写 `config.toml` 时，**必须以 ship 装进来的那份为底**：

```bash
cp "$HOME/.gongga/ship/config/config.toml" "$home/config.toml"
# 再把 disk_low_mb 插到 [server] 下面
```

只写 `[server] disk_low_mb = …` 的 home 没有 `[tool_text]` 那 40 段句子，服务端会直接拒绝
启动（`config.toml words nothing about ["bash (what it is)", …]`）。从脚本外面看，症状是
`the server never listened on 127.0.0.1:<port>`，很容易被当成端口或进程问题。

## 改了什么（第二版）

- `crates/server/src/disk.rs`：`Watcher { threshold, home }`（进程内一份），`note_tool_end`
  读卷、比阈值、只在边沿写一行 `events(kind='disk_space')`；`free_space` / `volumes` /
  `human` / `low` 不变。
- `crates/server/src/tools/mod.rs`：`invoke` 里**无条件**调用 `note_tool_end`（原来只在
  `!outcome.ok` 时调用）。
- `crates/server/src/config.rs`：`[server] disk_low_mb`（默认 1024），`GONGGA_DISK_LOW_MB`
  覆盖；原来的 `disk_reserve_mb` 连同 `disk::Reserve` 一起删掉。
- `crates/server/src/monitor.rs`：`Kind` 表不变；删掉 `arm_again`；磁盘报告改写为
  "还没失败，但空间已经低于阈值"。
- `scripts/disk_selftest.sh`（新）+ `scripts/monitor_selftest.sh`（只剩 error 那一支）。
- `docs/notes/server-disk.md`：按阈值重写。

## 没做的、留给下一个人的

- **SPEC 里仍然没有 monitor**：note 引的那段 SPEC 文字现在不在 SPEC.md 里。要不要补一节，
  等用户点头。
- `--remote` 会话：cwd 在对面的机器上，那一支只看本机的卷。
- 报告只带"这次边沿"那一次快照；如果清理没腾出空间，再报要等盘回到阈值以上。这是刻意的：
  没有第二个数（"又掉了多少再报"）就不发明一个。

## 用户后来定的（远端磁盘，2026-11-05 第二轮）

> 那就 monitor 用本地 monitor，notice 里根据目标机器决定是否注入 remote skill

所以：

1. **`@monitor/disk` 永远跑在本机**（`ensure_on` 不给 `--remote`）——它的 `bash`、`read_file` 都在本机，
   `@skill: clean-disk` 的 `.ps1` 也只在本机有意义。
2. **报告按目标机器注入参考**：本机低 → `@skill: clean-disk`；远端某台低 → `@skill: clean-disk` +
   **`@skill: remote`**，并在正文里写明机器名、它的卷表、阈值，以及"动手要经过 remote skill 的
   `run`/`put` 到那台机器"。
3. 事件按机器分开（`payload.machine`：`local` 或远端名），一次"落下"一台机器一条——一台低不掩另一台。
4. 远端读数：`remote/protocol.rs` 加 `Op::Space { path }`；`runner.rs` 在**它自己那台机器**上调
   `crate::disk::volumes()`/`free_space()` 回答最小剩余空间 + 卷表；服务端在远端 tool 调用结束时
   从 `ctx.reader` 的复用连接发一条请求（一次往返，不碰会话 shell），再与同一个
   `[server] disk_low_mb` 比较。
5. 未做：`main` 仍缺这轮全部工作（见下）。补它需要在 `docs/notes/server-monitor.md` 与
   `scripts/monitor_selftest.sh` 两处解冲突：`git merge fix/older`（我的磁盘文段 + 他们的新增）。
