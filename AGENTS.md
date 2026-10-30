- SPEC is the only source of truth (agents should not edit without user's permission).
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
  comments-check --config comments-check.toml
  ```
## 发布（改过 ship 内容之后）

`ship/` 是内容仓库在本仓库里的 submodule，所以「发布」有两个名字要一致：远端的 commit 和
`crates/server/ship.toml` 的 `commit`。`scripts/ship.sh` 一次做完，并校验三处一致：

```bash
scripts/ship.sh --check                # ship HEAD / 远端 main / ship.toml 是否说的是同一个 commit
scripts/ship.sh --dry-run -m "..."     # 会做什么（不写不推）
scripts/ship.sh -m "..."               # 提交 + push ship + 把 sha 写进 ship.toml + 这里 git add
scripts/ship.sh -m "..." --push-main  # 再把本仓库的 pin 提交并 push
```

- 有改动时 `-m` 必填：脚本不替你编一条发布说明。
- 远端 main 上有 ship/ 没有的提交就拒绝 push，从不 force-push；`--check` 会把这种漂移说清楚。
- pin 是 `include_str!`，所以改完要 `cargo build -p gongga-server` 才生效，运行中的服务端还得 `evolve`。
- 它自己有测试：`bash scripts/ship_selftest.sh` 拿一个临时 bare 仓库当远端，跑通提交、push、写 pin、
  漂移拒绝全过程，不碰 GitHub。

## 自我进化（改服务端之后怎么让自己用上）

整套流程在 `evolve` skill（`$GONGGA_HOME/skills/evolve/SKILL.md`），脚本是它旁边的
`evolve.sh`：

```bash
S=$GONGGA_HOME/skills/evolve/evolve.sh
$S stage        # 跑测试 + 构建 + 暂存 + 生成 launcher（不重启任何东西，先报磁盘）
$S stage --clean # 同上，并让继任者在旧服务端退出后跑 cargo clean（磁盘紧张时用）
# 然后用 bash --detached 启动它打印的 launcher
$S swap <stamp> # 确认继任者真在等，再发 cancel
$S status       # 当前 stamp、谁在等、端口与锁
$S clean        # 收掉没人会 swap 的旧继任者（--artifacts 连旧暂存件）
```

三条必须记住的：

- **先起继任者，再 cancel**。反过来一旦新二进制起不来，home 就空着、没人 resume 本回合。
- **不要写交接笔记**。resume 会把完整 transcript 原样重放，笔记不会多提供任何信息；
  要验的是外部状态（pid / 端口 / pending），那部分只能重新测量。
- **换完看继任者自己的日志**有没有 `replaying a turn ... resumed=true`，那才是"接上了"。

## 技能
skill 是"任务相关才读"的散文 + 脚本，**不在这个仓库里**：它们由独立的内容仓库
ship，服务端 pin 一个 commit，启动时装进两个根——harness 桶进 `$GONGGA_HOME/skills/`，
通用桶进 `~/.agents/skills/`。

**别全读，也别 grep 目录**：调用 `index_skill` 工具，它返回 name / description / path /
bucket 一张表，命中才按它给的路径读正文。列表本身不写在这里——写在这里就等于每加一个
skill 都要改一次这个文件，而读的人手上已经有那张表。

内容仓库现在是这个仓库的 **submodule**，就在 `ship/`——所以「改 skill / 改默认 config」
和「重新 pin」是同一件事：在 `ship/` 里改、在那边提交并 push，然后 `git add ship`（记下
新的 commit）+ 把 `crates/server/ship.toml` 的 `commit` 改成同一个 sha。两处不一致时服务端
装的是 `ship.toml` 里那个 commit，不是你工作树里的那个。
在这里改 `.agents/skills/` 是改一个已经不存在的地方；改 `$GONGGA_HOME/skills/` 里的已安装
副本会被下一次合并判成「读者自己写的」而不再更新。
- `proxy`：两层。`~/.agents/skills/proxy/proxy` 是自实现的纯标准库分流代理（HTTP/SOCKS5/直连
  上游，用户可接任何 API 端点）：先 `doctor`，`upstream add` 加上游，`add host --via` 定规则，
  `check host` 看代理自己记下的去向。出口 IP 只在两条路由相同时才说明问题——`check` 会明说。
  改配置后要重启 `serve`。`proxy core …` 直接驱动跑着的 mihomo/clash core：`groups` /
  `nodes` / `use GROUP NODE` 换节点、`bench [--throughput]` 测延迟和吞吐、`mode` 切
  rule/global/direct、`reload` 热重载。控制器自己从 core 的 config.yaml 找（不依赖任何
  客户端），`doctor` 会打印用的是哪一条来源。**换节点不用 flm；只有刷新订阅那一步需要账号。**
- `computer_use`：用鼠标键盘操作真实桌面。`~/.agents/skills/computer_use/cu` 先跑 `doctor`，
  然后 `shot` 看屏 → `tree` 拿控件矩形 → `click`/`type`/`key`（都带 `--window`）。
  `cu.py` + 每平台一个后端（Windows ctypes/UIA 已实测，macOS/Linux 已写未实测），
  uv 解析内联依赖，不需要 pyautogui/pywinauto。这是用户的真实桌面，打字前先确认目标窗口，
  截图会把屏幕上的密钥一起带走。
- `remote`：在另一台机器上跑命令、搬文件。`~/.agents/skills/remote/remote` 是纯标准库的 SSH
  外壳，目标由 `add` 记住（host/port/user/key），**不读 ~/.ssh/config，不依赖任何本地约定或
  第三方 CLI**。先 `doctor` 证明这条链每环（退出码/UTF-8/二进制/传输），再 `run`。脚本走
  base64 落盘执行、输出按 base64 分帧——所以引号、GBK、退出码这三个坑不存在。会话复用一条
  ssh（~390ms vs 一次性 ~800ms）；带 `--runner`（如 Windows 上跑 Git Bash）的目标没有会话，
  自动走一次性。传文件用 `put`/`get`，默认读回大小核对——sftp 会报告成功而文件没到。
- `heir`：把上下文交接出去。收到 `Context notice` 就是**要求尽快交棒**，不是询问——但也不是立刻
  砍断：先提交、把只存在脑子里的状态写下来、把 git 状态整理好，再交棒；不要再开新活。用
  `$GONGGA_HOME/skills/heir/transcript`（脚本，不是服务端工具）——服务端只测量和要求。
  `heir` 先把整份 transcript 拷进一个 mentor 会话、逐行校验副本，然后**删空自己**——只留在
  飞的那个 tool call（服务端一收到 call 就执行，所以它总在最后一行），指引就写在它的结果里，
  不再多插一行；mentor 那边会补一条 tool_result 告它自己成了 mentor，只答被问的。
  之后卡住了用 `get_messages` 读 mentor、用 `send_message` 问 mentor。
  read 读的是消息（模型输出 + 注入），工具调用与 reasoning 被过滤掉。
