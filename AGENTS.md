
**WRITING TEST IS NOT ALLOWED**
**CODE THAT WILL PANIC IS NOT ALLOWED**

- @file: SPEC.md is the only source of truth (agents should not edit without user's permission).
- @skill: whiting-dev is developing standard.
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

### 验证要快：三层，每层只付一次

慢的从来不是验证本身，是**每改一行就重新付一遍全栈的钱**（重新链接、重新起服务端、
重新 sleep、重新读日志判断）。

| 层 | 花多少 | 什么时候跑 |
| --- | --- | --- |
| L1 `cargo test --workspace` | 一次几秒 | 每轮，**一次跑完整个 workspace**，不要按 crate 拆成十次 |
| L2 变异检查 | 一次半分钟 | 每加一条测试：把修复撤掉，看它是否变红，再恢复 |
| L3 真进程 e2e | 一次几十秒起 | 只在单测**够不着**的地方：信号/进程生命周期、真实 `.db` 的迁移、跨进程握手 |

优先用 L2。本轮第一个真 bug（改名后 `NOT NULL` 挡住插入）是 L2 抓的：半分钟。
第二个（窗口与它自己 spawn 的服务端 key 不一致）只有 L3 能看见——但当时我拿 L3 当 L1 用，
每轮几十秒、pid 读错、key 用错，三轮才把它逼出来。**L3 该省的是轮数，不是不该跑。**

### L3 写成脚本，别每次手写 heredoc

参考 `scripts/heir_selftest.sh`、`scripts/ship_selftest.sh`、`scripts/watchdog_selftest.sh`、
`scripts/monitor_selftest.sh`、`scripts/prompt_selftest.sh`：它们自带临时 home、自己收尾、
一行 verdict。硬性几条：

- **pid 从 greeting 或 `server.lock` 读，永远不要 `$!`**。Git Bash 的 `$$` 是 MSYS 编号，
  Windows 的 pid 是另一套；两者混用会得到「进程还活着」这种假失败。
- **就绪用 dial 判断，不要 `sleep` + 探端口**。服务端回 greeting 才是「能干活」。
- **临时 `GONGGA_HOME` + 端口让内核分配**，并按 session 名或 worktree 名加门牌；
  多 agent 共机时固定端口和固定 `/tmp/xxx` 就是互相踩。
- **key 显式传**。`GONGGA_KEY` 起的服务端**不写** `server.key`，只查 home 会拿到空。
- **`trap` 里 kill + `rm`**，否则下一次删不掉 `gongga.db`（busy）。
- **一行 verdict**：`PASS/FAIL · 耗时 · build sha`，别让下一个 agent 读日志自己判断。

**一个服务端生命周期跑完 N 个场景**：写成 `scenario | args | expected` 的表，
一次起服务端、跑完全表、一次收尾。进程生命周期类的场景（关窗口 → 服务端停）没法共用，
另开一个脚本，别混进去。

### 多 agent 共机的硬约束

- **每个 worktree 自己的 `CARGO_TARGET_DIR`**。共用一个 `target/` 时并发 cargo 排队等锁，
  而且 Windows 锁着正在跑的 `.exe`，会直接 `拒绝访问 (os error 5)`。
- **`.dev/` 会长到 GB 级**：`evolve.sh stage` 把构建放 `.dev/target`（为了绕开 exe 锁），
  历次 `*-home/` 各带一个 `ship.git`。验证脚本自带清理，或别在这里攒。
- **不要并发跑 `evolve`**：它靠 `.dev/evolve-current` 单指针决定谁接管 home，两个 agent
  同时 stage 会互相判成被 supersede。

### 绝不按镜像名杀进程

**`tasklist | grep gongga-server` 然后 `taskkill` 那一批，等于杀用户正在跑的服务端。**
2026-11-02 就这么干过一次：排除用的 pid 是几秒前读的，期间服务端被重新拉起、pid 变了，
于是它落进「其余全部」，把用户那一回合连同服务端一起打死。同时跑着的服务端在名字上
**没有任何区别**——同机多 worktree、同机多构建，`gongga-server.exe` 是同一个字符串。

只认 pid，且只认这三种来源：

| 要杀的 | pid 从哪来 |
| --- | --- |
| 测试服务端 | 该测试 home 的 `server.lock`（**不是 `$!`**，也**不是**镜像名） |
| 它的 watchdog | `Get-CimInstance Win32_Process -Filter 'ProcessId=<上面的 pid>'` 的 `ParentProcessId` |
| 测试用的 python（fake LLM 等） | 让它自己写文件：`open(pidfile,'w').write(str(os.getpid()))`。Git Bash 的 `$!` 是 MSYS 号，`taskkill` 杀掉的是另一个进程 |

补充几条，都是上面那条的推论：

- **判「是不是我的」用 home / 端口，不用名字**：`server.lock` 里的 pid 与
  `--home` / `--bind` 才是身份。清理前先确认那个 pid 的 `CommandLine` 里是**你的**临时 home。
- **`taskkill /T` 会带走整棵树**：服务端的 shell 是它的子进程，杀之前想清楚这一棵里有没有
  你不该动的东西（用户的 detached job 就挂在服务端下面）。
- **宁可留一个测试进程，也不扫一片**：清理失败就报出来，交给下一个 agent 或用户；不要
  「顺手都清了」。
- **`rm` 掉不掉 `.db` 是信号**：`Device or resource busy` 说明还有进程握着那个 home。
  这时去杀进程，不要去 `rm -f`。

### 结果落成数据，不是笔记

`.dev/verify/<sha>-<area>.json`：build、命令、场景、结果、耗时。下一个 agent **先读**，
同一个 sha 命中就引用，只有 sha 或场景变了才付 L3 的钱。

这与「不要写交接笔记」不冲突：笔记是散文，会撒谎；这里每条的失效条件是写死的（sha 或
场景一变就重测）。**散文不写，测量要存。**

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
- `--check` 的退出码是契约：`0` 一致、`1` 不一致（真漂移）、`2` 用法、`3` **远端没应答**（只有
  "这个 pin 能不能被取到"还没法判断）。第三种不是不一致，别把它当成不一致处理。
- pin 是 `include_str!`，所以改完要 `cargo build -p gongga-server` 才生效，运行中的服务端还得 `evolve`。
- 它自己有测试：`bash scripts/ship_selftest.sh` 拿一个临时 bare 仓库当远端，跑通提交、push、写 pin、
  漂移拒绝、以及远端不可达与真漂移的区分全过程，不碰 GitHub。

**提交时挡住"pin 指向一个没人发布过的 commit"**：`scripts/hooks/pre-commit`。它只在这次提交
动了 `ship` 或 `crates/server/ship.toml` 时才跑（其余提交不发网络请求），不一致就拒绝；远端不可达
只警告、不挡（提交不能依赖网络）。每个 clone 装一次：

```bash
git config core.hooksPath scripts/hooks
```

为什么值得挡：`ship.toml` 里写一个只存在于本工作树的 sha，新机器取不到它——而
`install_at` 曾经对着取不到的 commit 报 `written: 0, fetched: true`，于是 home 里没有
`config.toml`，几百行之后才以 "no prompt templates" 报出来，指向 home 而不是 pin。

**推之前再挡一次**：`scripts/hooks/pre-push`。它问的是这次要推的**分支尖端**，不是提交的范围——
merge、squash、rebase、cherry-pick 造出来的提交照样被问，包括只在冲突解决里存在的那个 pin。
普通推送只多一次本地 `for-each-ref`；只有本机从没见过的 pin 才去问内容仓库。推送本来就要有网，
所以这里**答不上就不推**（与 pre-commit 的"离线放行"相反）。它挡不住 `--no-verify`、没装 hook 的
clone、以及任何不是这个 git 发的推送（网页、bot）——那些只有接收侧能挡，靠 `gongga-ship` 的 main
禁 force-push、禁删分支。

## 改过服务端代码之后

跑着的进程还是旧二进制，新代码要接管这个 home 才算生效。整套流程在 `evolve` skill 里
（`index_skill` 会给出路径），**不在这里重述**：那份散文和 `evolve.sh` 一起版本化，写第二遍
就一定会有一份是旧的。

这里只留一条不随 skill 变的：改 server 代码的人**必须**走完它，否则你自己、下一个 agent、
以及用户手上的窗口都还连着旧 build——症状是"代码改了但命令目录还是旧的"。

## LLM 目录（`~/.agents/providers.toml`）

provider 目录分两层，服务端只读不写，启动时按 provider `id` 字段级合并，私有覆盖社区：

| 根 | 谁的 | 怎么来 |
| --- | --- | --- |
| `~/.agents/gongga-agents/` | 社区，`windwhiterain/gongga-agents` | 服务端缺失就 clone，启动时 + 每小时 `pull --ff-only` |
| `~/.agents/` | 用户私有 | 可以整个缺席：要覆盖哪个字段再写哪个 |

两层同构：`providers.toml` + `providers/`，所以 `quota_query.script` 的相对路径（`providers/deepseek.py`）
在哪一层写着就指向哪一层的脚本，不需要额外的查找表。

**社区那份永远不带密钥**：它公开，provider 只用 `key_env` 指环境变量。改 provider 的事实
（模型、上下文、模态、价格、端点）去社区仓库改并发 PR。密钥的优先级是环境变量 > 私有文件里
的 `key`，所以一台机器可以**没有私有文件**：13 个密钥在用户环境变量里（`DEEPSEEK_API_KEY` 等，
与 `key_env` 同名），社区那份就是唯一的 roster。只有要改 `base_url`、加自建 provider 时才写
`~/.agents/providers.toml`，那个文件只留本地、不进任何仓库。
`GONGGA_PROVIDERS_CATALOG=<path>` 仍然表示「就这一个文件」，两层合并整个跳过。

## 技能
skill 是"任务相关才读"的散文 + 脚本。三个桶，**越近的越先**：

| 桶 | 在哪 | 谁的 | 怎么改 |
| --- | --- | --- | --- |
| `project` | `<会话 cwd>/.agents/skills/<name>` | 这个 checkout | 直接改，随仓库走 |
| `harness` | `$GONGGA_HOME/skills/<name>` | 内容仓库 `ship`，pin 一个 commit | 改 `ship/skills/<name>` 再发版 |
| `general` | `~/.agents/skills/<name>` | 社区 clone `gongga-agents` | 改社区仓库 |

**别全读，也别 grep 目录**：调用 `index_skill` 工具，它返回 name / description / path /
bucket 一张表，命中才按它给的路径读正文。列表本身不写在这里——写在这里就等于每加一个
skill 都要改一次这个文件，而读的人手上已经有那张表。

**需要源码的 skill 不属于 `ship`。** `evolve` 要往上找 `Cargo.toml`、要编服务端、要读
`.dev/`，装进 `$GONGGA_HOME` 之后它找不到仓库根，所以它住在**这个仓库**的
`.agents/skills/evolve/`，随 checkout 走，不随 pin 走。**不要往 `$GONGGA_HOME/skills/` 里放它的
副本**：那份副本会随源码一起变旧，而且它自己 `cd` 到 home 之后往上找不到 `Cargo.toml`，一行
都跑不通——但 `index_skill` 会把两个同名 `evolve` 都列出来，读的人先撞上坏的那个。

其余（`heir` / `image` / `self-control` / `subagent` + 默认 config）在内容仓库里，是这个仓库的
**submodule**（`ship/`），所以「改 skill / 改默认 config」和「重新 pin」是同一件事：在 `ship/` 里
改、在那边提交并 push，然后 `git add ship` + 把 `crates/server/ship.toml` 的 `commit` 改成同一个
sha。两处不一致时服务端装的是 `ship.toml` 里那个 commit，不是你工作树里的那个。
`ship/skills/<name>` 就是一层，没有 `harness/` 那一层——它原样映射成 `<home>/skills/<name>`。
改 `$GONGGA_HOME/skills/` 里的已安装副本会被下一次合并判成「读者自己写的」而不再更新。

**跑一个 skill 的脚本：从它自己的目录出发，别写桶的路径。** 正文里写入口用
`SKILL_DIR=<本 SKILL.md 所在目录>`（`@skill:` 展开的 `SKILL.md: <path>`、`index_skill` 的
`path` 列都给得出它），再 `"$SKILL_DIR/<entry>" …`：home 会搬、桶会变，而别的 harness 不认
`<home>/skills/...` 也不认 clone 路径——它读到的是一个不存在的命令。gongga 里更短的一层是
`gongga-cli skill <name> [args…]`（`skill ls` / `skill which <name>` 是它的两个诊断动词），
它按同样的三根优先级晚绑定解析，参数原样透传，退出码就是入口的。入口本身在 frontmatter 里
声明（`metadata.entry:`，规范 `metadata` 允许的扩展位），所以正文和机器读的是同一份事实。
`scripts/skill_contract.sh` 拦两种走样：正文出现桶路径、`entry` 指不到文件。

- `proxy`：两层。它自己的 `proxy` 脚本是自实现的纯标准库分流代理（HTTP/SOCKS5/直连
  上游，用户可接任何 API 端点）：先 `doctor`，`upstream add` 加上游，`add host --via` 定规则，
  `check host` 看代理自己记下的去向。出口 IP 只在两条路由相同时才说明问题——`check` 会明说。
  改配置后要重启 `serve`。`proxy core …` 直接驱动跑着的 mihomo/clash core：`groups` /
  `nodes` / `use GROUP NODE` 换节点、`bench [--throughput]` 测延迟和吞吐、`mode` 切
  rule/global/direct、`reload` 热重载。控制器自己从 core 的 config.yaml 找（不依赖任何
  客户端），`doctor` 会打印用的是哪一条来源。**换节点不用 flm；只有刷新订阅那一步需要账号。**
- `computer_use`：用鼠标键盘操作真实桌面。它自己的 `cu` 脚本先跑 `doctor`，
  然后 `shot` 看屏 → `tree` 拿控件矩形 → `click`/`type`/`key`（都带 `--window`）。
  `cu.py` + 每平台一个后端（Windows ctypes/UIA 已实测，macOS/Linux 已写未实测），
  uv 解析内联依赖，不需要 pyautogui/pywinauto。这是用户的真实桌面，打字前先确认目标窗口，
  截图会把屏幕上的密钥一起带走。
- `remote`：在另一台机器上跑命令、搬文件。它的 `remote` 脚本是纯标准库的 SSH
  外壳，目标由 `add` 记住（host/port/user/key），**不读 ~/.ssh/config，不依赖任何本地约定或
  第三方 CLI**。先 `doctor` 证明这条链每环（退出码/UTF-8/二进制/传输），再 `run`。脚本走
  base64 落盘执行、输出按 base64 分帧——所以引号、GBK、退出码这三个坑不存在。会话复用一条
  ssh（~390ms vs 一次性 ~800ms）；带 `--runner`（如 Windows 上跑 Git Bash）的目标没有会话，
  自动走一次性。传文件用 `put`/`get`，默认读回大小核对——sftp 会报告成功而文件没到。
- `heir`：把上下文交接出去。收到 `Context notice` 就是**要求尽快交棒**，不是询问——但也不是立刻
  砍断：先提交、把只存在脑子里的状态写下来、把 git 状态整理好，再交棒；不要再开新活。用
  `gongga-cli skill heir read`（脚本，不是服务端工具）——服务端只测量和要求。
  `heir` 先把整份 transcript 拷进一个 mentor 会话、逐行校验副本，然后**删空自己**——只留在
  飞的那个 tool call（服务端一收到 call 就执行，所以它总在最后一行）；指引写在它的结果里，
  第一课则由脚本用一次 `send_message` 发出，在下一个 step 边界落成一条普通 user 消息（所以它的
  `@file:`/`@skill:` 会进 index，和空会话的首轮一样）；mentor 那边会补一条 tool_result 告它
  自己成了 mentor，只答被问的。
  之后卡住了用 `get_messages` 读 mentor、用 `send_message` 问 mentor。
  read 读的是消息（模型输出 + 注入），工具调用与 reasoning 被过滤掉。
