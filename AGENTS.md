- when designing new feature, search any existing wheels. 

## 进程与构建纪律（Windows）

- 不要用 `cargo run` 启动长驻进程（server、TUI client）。用 `scripts/dev.sh`，
  它把二进制**复制**到 `.dev/` 下用唯一名字启动，因此 `cargo build` 可以随时覆盖
  `target/` 里的文件 —— 从根上消除「为了重新构建而杀进程」的动机。
- 只在用户明确要求时才终止进程。绝不 `taskkill` 不认识的 PID；`dev.sh stop`
  只杀 `.dev/*.pid` 里记录的、且经 `tasklist` 核对名字确实是 gongga 的进程。
- 用户正在交互的 TUI client 属于「用户的进程」，任何情况下都不得终止。它在独立窗口
  里，dev.sh 也不会追踪它。
- 若构建报 `拒绝访问 (os error 5)` 且怀疑是文件锁：先向用户报告 PID 与进程名并等待
  确认，不要自行强杀。优先尝试 `dev.sh build`（构建 target/，不需要停运行中的副本）。
