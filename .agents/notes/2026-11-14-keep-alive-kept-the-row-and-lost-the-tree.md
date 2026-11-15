# keep_alive 保住了那一行，没保住那棵树

2026-11-14 21:19，一次 `evolve.sh swap` 把 home 的旧服务端 cancel 掉，继任者（pid 62740）
同时消失。`.served` 从没出现，`launch.pid` / `launch.beat` / `evolve-lease` 全留着——
它死在等待循环里，连 `exec "$NEW" server` 那一步都没走到。home 空到现在。

## 断在哪

SPEC.md:143 写得很死：

> `cancel(keep_alive: bool = false)` … by default kill all channel, if keep_alive,
> make every channel detached, restart will adopt them.

实现只做了前半句：

```rust
let channels = match keep_alive {
    true => None,                                        // 只跳过了 stop_every
    false => Some(d.agent.channels.stop_every().await),
};
```

"detached" 不是一个数据库事实，是一个 job object 事实。relay 在 `channel.rs:393`
被 `tools::job::arm()` 放进 `ProcessJob::adopt_closed()`——`KILL_ON_JOB_CLOSE` 且
**不允许 breakaway**（因为 cygwin 的 fork 会带 `CREATE_BREAKAWAY_FROM_JOB`，允许了
就只锁住 leader 锁不住树）。于是：

- 服务端进程退出 → `ARMED` 里的 job 句柄关闭 → relay 被杀；
- relay 的 job 关掉 → 它锁住的 channel bash 被杀；
- **`evolve.sh launch` 正是把继任者交给一个 channel 起的**（`bash/idle2`，pid 71884），
  于是继任者一起被杀。

放行的函数早就写好了，注释还写着"cancel asked by a client is the only caller"：

```
$ grep -rn release_detached crates/
crates/server/src/tools/detached.rs:1164:pub fn release_detached() -> usize {
crates/server/src/tools/detached.rs:1170:pub fn release_detached() -> usize {
```

两处都是定义，**零调用点**。

## 现场

`~/.gongga/server.log` 21:19 那几行：

```
13:19:04.747  tools: the wait ended before the call answered session=status tool=gongga_cli why="cancel" elapsed=38.7162ms
13:19:04.754  WARN commands: the server is stopping: every turn was interrupted running=5 by=a tool in session status
13:19:05.130  net: server stopped
13:19:05.243  main: runtime dropped
```

38ms 的 `gongga_cli` 就是 `swap` 发的 `{"name":"cancel","args":{"keep_alive":true}}`——
不是人点的取消。继任者的 beat 停在 21:19，和 `server stopped` 同一秒。

## 补的门

`cancel()` 的 keep_alive 分支现在真的放行：

```rust
true => {
    let released = crate::tools::detached::release_detached();
    if released > 0 { tracing::info!(released, "cancel: every background tree was let go"); }
    None
}
```

对照验证（`.dev/keepalive-probe.sh`，临时 home、内核分配端口、一个 `sleep 300` 的 channel）：

| 二进制 | cancel 后 channel 程序 | relay |
| --- | --- | --- |
| 修复前 | `0/1 alive` | none |
| 修复后 | `1/1 alive` | 65568 仍在 |

## 顺带改掉的两处守卫

`evolve.sh` 里两个"永远为真"：

1. **端口判据是子串**：`netstat -ano | grep -c ":$PORT"` 会把客户端重连的
   `SYN_SENT` 行算进来（那一行的**远端**地址是这个端口）。窗口一重连，继任者就永远
   读到"端口还占着"，等一个早就不在了的东西。改成只看第二列（本地地址）以
   `:$PORT` 结尾。
2. **lease 永不放手**：`lease_live` 的第三个条件是"stamp 是当前的 + `stamp.txt` 存在"，
   没有上界。继任者死了、`launch.pid` 还残留，`status` 照样说 "is waiting to take the
   home"，下一次 `stage` 被 `another handover is in flight` 永久挡死。现在只有
   `outside`/`pid`/`beat` **都不存在**时才算"stage 完还没 launch"的窗口。

`scripts/evolve_selftest.sh` 改后全绿（`PASS · 28s · cff6b83d`）。

## 没查完的

探针第 5 项（下一次 start adopt 这个 channel）仍然红，而且红了两次、修复前后一样：

```
13:47:06  detached jobs adopted ... adopted=0
          watchdog: the server stopped without saying why (exit code 1); restarting in 200 ms
13:47:14  channels the last run kept were gone when adopted gone=1
```

同一个 home 的**第二次** start 会 `exit 1` 且不打印任何原因，watchdog 再起一个才跑到
`adopt`，那时 relay 已经退出并写了 `status.exit`。探针在 `sleep 1` 时取样还是
`exit=null`、relay 活着——所以 relay 是在 server-2 崩溃那几秒内死的。

它和 `release_detached` 无关（修复前后症状一致），是一条独立的线：**第二次启动为什么
exit 1，以及它退出时按什么规则收走了仍活着的 relay**。下一步该做的观测是：在 server-2
崩溃的瞬间同时记录 relay 的存活与它的父进程，而不是隔一秒取样。
