# 停下来的服务器不等那两个开局任务

## 现象

关掉 TUI 后 server 不退。日志停在：

```
02:29:28.232  the server is stopping: every turn was interrupted running=2
02:29:28.567  net: server stopped
02:29:28.587  main: dropping runtime
              ← 之后 33 小时没有任何一行
```

进程 4 个线程全在 `Wait`，5 秒内 CPU / 读写计数一动不动：死锁，不是慢。父进程（TUI）
已退出，watchdog 在 `child.wait()` 上等它。

## 成因

`lib.rs::main` 手动建 runtime，`run` 返回后 `drop(runtime)`。Tokio 的 `Runtime::drop`
会 **join 所有 `spawn_blocking` 任务**。启动时挂了三个“没有预算”的阻塞任务，其中
`ship::refresh` 会走 `ship::fetched()`，那是在**另一个线程**里跑 `git fetch` 然后
`rx.recv()` —— fetch 线程只要不 send，`refresh` 就永不返回，`drop` 就永不返回。

和之前 persistent shell 的 reaper 是同一类：没有终点的等待被放进了会被 join 的池子。

## 修法

按“替停下来的服务器省掉哪些等待”分类，而不是给 `drop` 加超时：

- **要等**：shell、脚本、channel 目录扫描 —— 这些是 `run` 启动并且欠一个结局的工作，
  仍然全在 runtime 的阻塞池里，`drop(runtime)` 照旧 join 到全部完成。
- **不等**：`ship::refresh`（内容仓 fetch）和 `agents::start`（社区 clone 的同步）。
  它们是**启动的后台活**，不属于任何请求，网络卡住时也没人能给它结局。各自用
  `std::thread::spawn` 起一条独立线程：不在 runtime 里，`drop` 看不见它们，进程退出
  时一起消失。所以代码里读到的 `std::thread` 是有意的，不是漏改成 `spawn_blocking`。

`shutdown_background()` 能一把绕开，但它连该等的也不等了（channel 扫描会被砍在中间），
所以只把那两个任务挪出去，其余保持 await。

## 验证

- `cargo check -p gongga-server` 通过，改动文件无警告。
- `mbx fmt --check`、`test-layout-check`、`skill_contract.sh`、`ship.sh --check` 通过；
  `comments-check` 回到基线 2 条。
- 本机 `target/debug/gongga-server.exe` 被两个残留 server 进程占着（不许强杀），
  因此没跑 L3 真进程自测；行为等价性靠 `run` 的收尾代码未变 + 线程归属变化保证。

## 遗留

残留的 84220（卡死的 server）/ 74252（它的 watchdog）不会自愈，需要手工结束 84220，
watchdog 看到子进程退出会自己走。新构建要生效需要重启 live server。
