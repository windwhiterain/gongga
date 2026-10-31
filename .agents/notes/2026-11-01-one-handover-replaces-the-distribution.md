# 一次换手换掉整套发行版（已完成）

User 的指令，2026-11-01：「能不能完善一下 evolve，让该换的都换了？自动化程度高一点？」
这条记的是**漏了哪三样**、���及现在的形状。

## 漏的三样

1. **payload 目录**：`stage` 只构建 server，换手只换进程；`~/.gongga/payload/<stamp>/`
   里另外两个程序停在昨天的构建。
2. **`<home>/bin` 里的一对**：`shell_command::ensure_command` 原来是「有就返回」，于是
   `bin/gongga-cli.exe` 一旦写进去就再也不更新。理由是「刷新是 CLI 的活」——而 CLI 正是换手
   不会运行的那个程序。症状是可复现的：服务端是新的、PATH 上的 `gongga-cli` 是旧的，新功能
   （sticky 等）看着「不生效」。
3. **命令找服务端的顺序**：`bin/gongga-cli` 与 `bin/gongga-server` 不同源时，一个旧命令能
   拉起一个旧服务端。

## 现在的形状

```
stage:  cargo test → cargo build -p gongga-bundle → 暂存 .dev/evolve-<stamp>-gongga.exe
launch: 等旧服务端退出（argv 在 stage 时已从旧进程读好）
exec:   GONGGA_HOME=<home> <bundle> server <旧 argv>
        → bundle 把三个程序 place 进 <home>/payload/<stamp>/
        → retire 别的 payload（正在被窗口使用的目录保留）
        → 跑刚 place 的 gongga-server.exe，端口/home/owner 全不变
server: 启动时 ensure_pair：把自己和同构建的 cli 装进 <home>/bin>（不是「有就算了」）
```

于是换手之后：payload 是新的三个程序、`bin` 是同一构建的一对、跑着的服务端是新的。
**唯一没换的是正在跑的窗口**（`gongga-tui.exe` 换不掉自己），但它的 payload 目录不会被删，
旧窗口照常工作到用户关掉它。

`stage --all` 随理由一起消失：所有程序本来就跟着走。

## 「同一构建」怎么判断

长度 + mtime，与 `cli::server` 刷新同一对时用的两个事实一致；两者都成立是因为装的是
**硬链接**而不是复制。正在运行的程序换不掉：那一次启动保留旧的，下一次再替换；永远看不到
半个程序（先写旁边再 rename）。

## 验证

- L1：`cargo test --workspace` 10 组全绿；`comments-check` 78 = 基线；`test-layout` 0 违规。
- L2：把 `place` 的刷新条件退回「文件存在就返回」，新增测试立刻红。
- L3（临时 home，从「旧发行版」起手）：旧 payload 退休、三个程序落地、服务端从新 payload
  运行且 `--parent` 仍在、`bin` 一对与新 payload 同尺寸、`bin/gongga-cli` 能驱动新服务端、
  **杀掉窗口后服务端随之退出**（SPEC 的 lifetime）。
- 真机：2026-11-01 00:21 换手，`payload/4875699e…` 三个程序 00:21 构建，`bin` 同尺寸，
  PATH 上的 `gongga-cli` 变成新构建，sticky `--model` 当场可用（设一次、下一条命令继承）。

## 仍不在 automation 里

- **窗口**：`gongga-tui.exe` 无法自我替换，用户重开窗口才用上新的 tui。
- **ship pin**：内容仓库的提交要 `scripts/ship.sh` 先 push 再回写 sha，本机没有网络时它留在
  未发布状态。
