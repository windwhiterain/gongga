# WIP: b2ded40 claw plan-finish bench (live_compact 三臂)

- bench worktree: /root/gongga-b2ded40 (WSL, from bundle, HEAD b2ded40 = lc-pf-fix 合入后)
- 上游 30a8937 服务端不应答命令(同 build client 也一样) — 按指示不追, 用 b2ded40 隔离
- build-only cfg 修复: crates/tui/src/clipboard.rs (not(windows) stub), diff 存 .dev/clipboard-cfg-fix-b2ded40.diff, 未提交
- payload 预处理: bundle place 后 chmod +x /root/.gongga/payload/*/* (Linux place() 不加执行位)
- GONGGA_BIN=/root/gongga-b2ded40/target/debug (gongga-cli->gongga, gongga-server; parent.parent=worktree root 使 harness _append_instructions 找到 ship/config/config.toml)
- harness mode 透传: bench/harness/gongga.py _set_session 读 GONGGA_SESSION_MODE (unset 不追加), 备份 gongga.py.bak-30a8937
- 三臂 (claw WT /root/claw-swe-bench/.worktrees/gongga-live-compact, django__django-11880, MiniMax-M3.1-Flash):
  - A gongga-lc0-work-11880-b2ded40  GONGGA_LIVE_COMPACT_ENABLED=false GONGGA_SESSION_MODE=work
  - B gongga-lc1-work-11880-b2ded40  enabled=true mode=work (interval 默认 20000, 不设)
  - C gongga-lc1-explore-11880-b2ded40 enabled=true mode=explore
  - homes: /root/claw-homes-b2ded40/{A,B,C}; keepalive: wsl sleep (session 存活期间保持)
- 结果落 C:\resource\gongga\.dev\verify\b2ded40-claw-plan-finish.json (记录 exit/resolved/requests/tools/input/cached/output/peak/checkpoint/mode转换/lc notice/call/error)
- 官方 eval 按 A→B→C; 收尾只清三 run 的 home/container/进程

## 结果 (2026-11-07 01:15 UTC)
- 三臂全 resolved (django__django-11880):
  - A lc0-work: exit 0, 773s, req 66, in 1.55M (cached 1.51M), out 22.8K, peak 41K
  - B lc1-work: exit 0, 1416s, req 71, in 2.16M (cached 2.11M), out 28.7K, peak 53K
  - C lc1-explore: exit 0, 757s, req 59, in 1.68M (cached 1.63M), out 22.4K, peak 46K, mode=explore
- lc 未触发（peak 未越过 20000 interval 生效条件）; checkpoints 0; mode 无中途转换
- 结果: C:\resource\gongga\.dev\verify\b2ded40-claw-plan-finish.json
- eval json: /root/claw-homes-b2ded40/evalwork/MiniMax-M3.1-Flash.gongga-lc*-b2ded40-eval.json
- raw artifacts: /root/claw-swe-bench/.worktrees/gongga-live-compact/artifacts/gongga-lc*-11880-b2ded40/django__django-11880/ (git.patch/report/server.log/db)
- 已清理: 三 home、keepalive; 容器由 orchestrator 自清; dockerd 保留
- bench worktree 未提交修复: clipboard cfg stub + python/gongga.py 分片缓存与分片发送 (diff: .dev/bench-local-fixes-b2ded40.diff)
- harness gongga.py 补丁(备份 .bak-30a8937): python client 入口/新语法/mode 透传/mtu=1000 无条件写
- 关键环境事实: WSL2 loopback 丢 >~1.5K UDP 数据报 -> home config [transport] mtu=1000 + python 分片; gongga-cli=包装脚本直起 gongga-server（绕开 bundle payload 执行位问题）
