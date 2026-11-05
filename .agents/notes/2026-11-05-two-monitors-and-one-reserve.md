# 两种 monitor，和一台设备共用的一份安全空间

日期：2026-11-05 · worktree `.worktrees/monitor-kinds` · 分支 `feature/monitor-kinds`

## 用户要的

> 现在只有 session error monitor，新增一种 monitor 来清理磁盘，已经有对应的 skill 了，
> 不同的 monitor 用 `@monitor/xxx` 名字。

然后是用户自己提的设计（本文照它实现）：

> 能不能让 server 在磁盘上预先占据一个安全空间，当工具触发 disk space error 时释放
> 安全空间然后唤醒？

> 但是有个问题，工具的报错可能在多层嵌套中丢掉了，跑一次剩余空间查询贵吗，能否在每次
> tool error 都检查？

> 整个 device 共享一个，多个 server 如果同时触发，如果发现贡献的安全文件被删，就抑制
> 这次触发。所以说安全文件的查看+删除能合并为原子操作吗

## 用户定的

1. 预留 **1 GiB 默认，config 可配**（`[server] disk_reserve_mb`，`0` 表示不预留）。
2. **每次 tool error 都查剩余空间**（一次 `statvfs` / `GetDiskFreeSpaceExW`，微秒级），
   文本匹配只作提示，不作判据 —— 因为报错会被管道、`2>/dev/null`、本地化吞掉。
3. 安全文件**整台设备一个**，多个 server 共享；看到文件已被别人删掉就抑制本次触发。
4. 查看+删除合并为原子操作 —— 见下，Windows 上不是 `DeleteFileW`。

## 学到的一件事：Windows 上"删掉"不是 compare-and-swap

`disk::tests::two_servers_share_one_claim_and_only_one_wins` 跑到第四轮就红：两个线程
对同一个文件调 `remove_file`，**两个都返回成功**。同一个 race 的另一个方向返回
`拒绝访问 (os error 5)`，而调用方没法把它和"真出错了"分开。

于是 claim 改成**独占打开**：`CreateFileW(DELETE, share = 0, FILE_FLAG_DELETE_ON_CLOSE)`。
第一个调用者拿到句柄，其余拿 `ERROR_SHARING_VIOLATION`；文件随赢家的句柄关闭而消失。
unix 仍然是 `unlink`（输家拿 `ENOENT`）。改完 16 轮 × 12 次全绿；把 claim 改回
`remove_file` 重跑，第 4 轮就红 —— 这是那次 L2 变异检查。

16 轮写进测试，是因为 1/4 概率的 race 会在别人机器上丢。

## 改了什么

- `crates/server/src/monitor.rs`：`Kind { session, skill }`，`ERROR`（`@monitor/error`，
  skill `monitor`）与 `DISK`（`@monitor/disk`，skill `clean-disk`）；`hand_over` 是两种
  共用的"选模型 → adopt → deliver"；磁盘那支的游标/重试存在 `monitor_state`。
- `crates/server/src/disk.rs`（新）：`Reserve`（`arm` / `claim` / `armed`）、`free_space`、
  `volumes`、`note_tool_failure`、进程内唯一的 `Watcher`。
- `crates/server/src/tools/mod.rs`：`Registry::run` 是所有工具调用的唯一漏斗，失败的
  在那里被读成"可能是一块满盘"。
- `crates/server/src/store.rs`：`monitor_state` 表、`monitor_value` / `set_monitor_value`、
  `event_row`。
- `crates/server/src/config.rs`：`[server] disk_reserve_mb`，默认 1024。
- `crates/server/src/lib.rs`：启动时 `disk::install` + `arm`。
- `docs/notes/server-disk.md`（新）+ `server-monitor.md` / `server-commands.md` /
  `server-session.md` / `README.md` 同步。
- `ship/skills/monitor/SKILL.md`：`@monitor` → `@monitor/error`。
- `scripts/monitor_selftest.sh`：加磁盘那一支（花掉预留 → 事件 → `@monitor/disk` →
  第一个模型被拒 → 换模型跑完 → 重新上膛）。

## 没做的、留给下一个人的

- **SPEC 里仍然没有 monitor**：note 引的那段 SPEC 文字现在不在 SPEC.md 里。要不要补一节
  （`@monitor/<kind>`、两种触发、预留空间），等用户点头。
- 会话带 `--remote` 时 cwd 在对面的机器上：那一支只看本机 home 的卷，远端磁盘不在监视范围。
- 安全文件只在它所在的那个卷上腾出空间；home 在另一块盘的 server 释放它，自己那块盘
  一字节没多。多盘要"每卷一个文件"时才需要改结构。
