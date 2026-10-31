# evolve 的继任者也归窗口管：谁连上来，谁就是 owner

日期：2026-11-01

## 症状：TUI 关了，服务端还在

用户问：「我关了 TUI，服务端应该被 cancel 啊，为什么还是老的？」

查下来 `:9700` 上跑的是 `59272`，命令行**一个参数都没有**：

```
C:\resource\gongga\.dev\evolve-20261031-163726-server.exe
```

不是窗口起的。它的父进程是 `bash .dev/evolve-…-launch.sh`（57432），也就是上一轮
会话的 `evolve.sh stage` → detached launcher → `swap` 那条链，launcher `exec` 出来的。
窗口那边每次都：

```
INFO providers: ... providers=12
Error: another gongga server is already running on this home (pid 59272 ...)
```

`InstanceLock` 拒绝，窗口的子进程当场退出。于是
`stops_a_server(spawned, answering)` 为假（`spawned != answering`），关窗只发 `goodbye`。

这是设计里的一条真空白：**`--parent` 只覆盖窗口自己起的服务端**。凡是从别的路径
进来的服务端（evolve 只是最常见的一条），没有任何东西看着它——窗口全关光，它照样
占着端口和 home 锁，下一个窗口想自己起也起不来。用户看到的「老的二进制一直跑」
只是这条空白的后果，`--llm` 没改名只是它上面一层灰。

## 修法：无主的服务端，认第一个连上来的窗口

协议里 `ClientInfo` 多一个 `owner: Option<u32>`：

- **只有窗口填**（`Endpoint::owning(std::process::id())`，TUI 每次拨号都填）；
- 一次性命令和 bridge 一律 `None`——它们毫秒级退出，填了就是把服务端带走；
- 服务端在握手成功后 `signal::adopt(pid)`：若**还没有 owner**，就把它当作
  `--parent` 一样跟随（同一个 `follows`，死了走 `commands::stop`，turn 存成 pending）。

三条不变量：

1. **先到者得**。第二个窗口连上来会被拒绝：两个 owner 意味着一个窗口关掉会带走另一个
   窗口正在读的服务端。
2. **`--parent` 先于任何连接**。窗口自己起的服务端在 `watch()` 里就 `OWNED=true`，
   所以 adopt 永远拒绝，两者不会同时生效。
3. **没有 notifier 就不认**（`WAKE.get()` 为空）。单测用 `claim(None, …)` 守着：
   「认了但没有东西可通知」比不认更糟。

## 为什么不让 evolve 自己传 `--parent`

launcher 是 `exec "$NEW"` 的，它会一直活着当服务端的父进程——用它当 owner，服务端
永远等不到那个死亡信号。真正的 owner 是用户正在看的窗口，不是 launcher。

## 验证

- 单测 `one_window_claims_an_unowned_server`（首claim、重复claim、已被 `--parent`
  拥有、无 notifier 四种）、`only_a_window_names_an_owner`（owner 过线、命令不过线）；
- 真机端到端（临时 home，用完删）：
  - 无 `--parent` 起服务端 → TUI 连上 → 日志出现
    `no owner yet: following the window that connected owner=58064`；
  - `kill -9` 窗口 → 服务端 `stopped as a signal asks` → 进程消失；
  - 同一条链路上跑 `gongga-cli set_session`：日志里 `no owner yet` 出现 0 次，服务端
    仍然活着——命令不是窗口。

## 没做

- 没让服务端「多 owner」或「最后一个窗口走才停」：那需要在协议里记连接的生命周期，
  而一个 home 一个服务端的前提是「谁开的谁关」，多 owner 会把这条变成投票。
- `DESIGN.md` 仍未更新（它是 `--llm` 时代的历史记录）。
