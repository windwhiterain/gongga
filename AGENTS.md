- SPEC is the only source of truth.
- when designing new feature, search any existing wheels. 
- setting any limit/budget is forbidden.
- session turn end should kill all process it spawns so user won't use a process that tied to a session as a long running process.
- server home for tests should be local to each worktree so worktree's servers won't conflict (otherwise will be locked).
- track your WIP under (notes)[.agents/notes/].

## Verification
- tests layout: 
  ```
  cargo install --git https://github.com/windwhiterain/test-layout.git test-layout-check --locked
  test-layout-check --config test-layout.toml
  ```
- comments test:
  ```
  cargo install --git https://github.com/windwhiterain/comments-test comments-check --locked
  comments-check --config comments-test.toml
  ```
## 自我进化（改服务端之后怎么让自己用上）

`scripts/evolve.sh` 把「换掉正在跑自己的服务端」按安全顺序做成了几条命令：

```bash
scripts/evolve.sh stage        # 跑测试 + 构建 + 暂存 + 生成 launcher（不重启任何东西）
# 然后用 bash --detached 启动它打印的 launcher
scripts/evolve.sh swap <stamp> # 确认继任者真在等，再发 cancel
scripts/evolve.sh status       # 当前 stamp、谁在等、端口与锁
scripts/evolve.sh clean        # 收掉没人会 swap 的旧继任者（--artifacts 连旧暂存件）
```

四条纪律：

- **先起继任者，再 cancel**。反过来一旦新二进制起不来，home 就空着、没人 resume 本回合。
  `swap` 会检查 launcher 的 pid 还活着，不活就拒绝 cancel —— 别绕过它手动 cancel。
- **只有最新的 `stage` 能接管 home**。`stage` 写 `.dev/evolve-current`；被取代的 launcher
  自己退出等待循环，`swap` 拒绝非当前的 stamp。所以重复 `stage` 是安全的，但别指望用旧
  stamp 回滚 —— 它的继任者已经走了，要回滚就重新 `stage`。
- **不要用 `target/` 构建待替换的服务端**：Windows 锁住正在运行的 `.exe`，而被替换的往往
  就是 `target\debug\gongga-server.exe`。`stage` 用 `.dev/target` 就是为了绕开这件事。
- **`stage` 默认只构建 server**：协议没变时旧客户端照样能连；要连客户端一起换用
  `stage --all`（但用户正在用的 TUI 会锁住 `gongga.exe`，见上一节）。

`cancel` 之后本回合被取消并保留，新服务端启动时会重放它 —— 所以「进化」不会吃掉正在进行的活。
代价见 `DESIGN.md` §10「仍然存在的风险」。
