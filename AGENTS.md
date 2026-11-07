
**WRITING TEST IS NOT ALLOWED**
**CODE THAT WILL PANIC IS NOT ALLOWED**

- @file(SPEC.md) is the only source of truth (agents should not edit without user's permission).
- @skill(whiting-dev) is developing standard.
- when designing new feature, search any existing wheels. 
- setting any limit/budget is forbidden.
- session turn end should kill all process it spawns so user won't use a process that tied to a session as a long running process.
- server home for tests should be local to each worktree: a shared home makes worktrees lock each other, and someone else's home is not yours to write in -- the program you run may stop the server on it.
- pointing at a home in use is not a read-only act: `GONGGA_HOME` on a live home makes the bundle place its payload there and retire the build it replaces, and that asks the home's server to `cancel` -- every turn it is running dies with the ask, unless the program starts no server of its own (as `--client_only` says). See `docs/notes/bundle-main.md`, "There is no read-only run".
- track your WIP under (notes)[.agents/notes/].

## Disk low：立刻跑脚本，一刻都不耽搁

**收到磁盘告警（free < threshold）时，第一动作就是跑 @skill(clean-disk) 的脚本，
不要先做任何调查、扫描、归因、计划。** 先释放，再解释。

- **第一动作**：`powershell -NoProfile -ExecutionPolicy Bypass -File "<clean-disk 的
  SKILL.md 同目录>/clean-disk.ps1"`（不弹 UAC 的用户级路径：见下）。
  **不许**在跑脚本之前先做 `Get-ChildItem -Recurse` 之类的体积扫描——磁盘已满时这些
  扫描会把机器拖到卡死，2026-11-06 就因此把一整轮拖成「脚本半天没跑」。
- **不许**因为「想先弄清楚是谁占的」而推迟清理。归因可以放在清理**之后**。
- 磁盘低于阈值时机器已经在卡：先腾出空间，让它能喘气，再谈分析。
- **一次告警只回一次**：低空间期间持续告警是同一件事，不要每条都重跑一遍。
- **报告用退出码和数字**（脚本汇总里的「实际释放」是权威值），不要用叙述顶替测量。

### 跑法：不弹 UAC 的用户级路径

脚本提权走 `Start-Process -Verb RunAs -Wait` + `-NoExit`，在无人值守的会话里会留下
一个提权窗口。改跑它**自身的正文**（等价于用户在 UAC 点「否」的路径，系统级项目跳过）：

```bash
powershell -NoProfile -ExecutionPolicy Bypass -Command \
  "Invoke-Expression (Get-Content -Raw '<clean-disk.ps1 的绝对路径>')"
```

### 已知缺陷：回收站那一步会虚报成功

`clean-disk.ps1` 的第 11 步是 `Clear-RecycleBin -Force -ErrorAction SilentlyContinue`，
紧接着**无条件**打印 `[完成] 回收站已清空`。实际它会抛
`Win32Exception: The system cannot find the path specified`（回收站里的路径 shell API
枚举不了），而错误被 `-ErrorAction SilentlyContinue` 吃掉。

**后果**：回收站常常是最大的一块（实测单次 16 GiB ~ 68 GiB），脚本报「已清空」而
文件还在。**不要相信那行输出，要自己量。**

回收站有几十万文件时它还会**挂死**在这一步（实测 68 GiB / 45 万文件挂了 20 分钟，
既不报错也不返回汇总）。所以：脚本跑到第 11 步就别等它——先并行跑下面的镜像，
镜像跑完脚本那一步也不会有用了，按 pid 把它收掉（pid 从 `Get-CimInstance` 的
`CommandLine` 认，不要按名字杀）。

清它要用脚本自己的 robocopy 空目录镜像
（长路径和 ACL 都吃得下），路径里的 `$` 要防 shell 吃掉：

```powershell
$empty = 'C:\resource\gongga\.dev\empty-mirror'; New-Item -ItemType Directory -Force -Path $empty | Out-Null
$bin  = 'C:' + [char]92 + [char]36 + 'Recycle.Bin' + [char]92 + '<当前用户 SID>'
& robocopy.exe $empty $bin /MIR /XJ /R:0 /W:0 /NFL /NDL /NJH /NJS /NP /MT:8
Remove-Item -LiteralPath $empty -Force -Recurse -ErrorAction SilentlyContinue
```

- 写 `'C:\$Recycle.Bin'` 会被 bash 展开成 `C:\.Bin`（`$Recycle` 变空），**静默扫到
  0 个目录**，看起来像「清完了」其实什么都没做。用 `[char]36` 拼 `$`。
- 回收站里若有你**不拥有**的目录（如别的账号装的 Anaconda），`icacls /grant` 会
  *Access is denied*——那是真需要管理员的一条，别反复试。

## Verification

### Invariant：门禁必须真的跑过

**任何一道检查存在，就必须在它守的那一步真的跑过；跑过的证据是退出码，不是"应该没问题"。**

- **不许关 hook。** 不用 `git -c core.hooksPath= commit`、不用 `--no-verify`、不删
  `scripts/hooks/`。觉得 hook 太慢、太吵、和这次改动无关时，先读它为什么在那儿；
  它拦的正是你以为无关的那一类。
- **hook 只在它认得的路径上生效。** `pre-commit` 只在这次提交动了 `ship` 或
  `crates/server/ship.toml` 时才判 protocol；`ship/` 里的提交是子模块自己的 git，外层 hook
  看不见——而内容已经不需要在本仓库留下提交了（`ignore = all`），所以"改了内容"与"本仓库
  有提交"从此是两件事。推送内容之后仍要 `scripts/ship.sh --check`：退出码是契约
  （0 一致 / 1 真漂移 / 3 远端没应答），它不是可选项。
- **报"做完了"之前，这一轮的每一道门都要有一次真实的执行**：`cargo build`、
  `cargo fmt --check`、`comments-check`（对着**基线**比，别只看总数）、
  `test-layout-check`、`scripts/skill_contract.sh`、`scripts/ship.sh --check`，
  以及单测够不着时的 L3 真进程脚本。没跑就说没跑，跑红了就说红了。
- **不能只凭"代码看起来对"结案。** 同一类结论至少要有一个可复现的观测：真帧、真进程、
  真 sqlite 行、`exit=N`、`sha`。这一轮就吃过教训——我为了省一次网络往返关掉了
  `pre-commit`，于是把一个"pin 指向未发布 commit"的提交记了进去；hook 一直是好的，
  是我没让它跑。现在没有 pin 了，但同一类事还在：内容推上去没有，只有 `--check` 说得出。

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

### 验证要快：一次真实的执行，别重付

慢的从来不是验证本身，是**每改一行就重新付一遍全栈的钱**（重新链接、重新起服务端、
重新 sleep、重新读日志判断）。

要看见的东西只有一种：真进程 e2e，一次几十秒起，只在别的手段看不见的地方用——信号/进程生命周期、
真实 `.db` 的迁移、跨进程握手、窗口里画出来的行。

本轮那个只有它能看见的 bug（窗口与它自己 spawn 的服务端 key 不一致）当初被我拿它当每轮的
便宜检查用，每轮几十秒、pid 读错、key 用错，三轮才把它逼出来。**该省的是轮数，不是不该跑。**

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

- **起服务端时 scrub 模型环境变量**（`env -u GONGGA_MODEL -u GONGGA_PROVIDER -u
  GONGGA_BASE_URL -u GONGGA_API_KEY -u GONGGA_API_KEY_ENV`）。从会话 shell 里跑测试时，
  这些导出变量（服务端现在会给 shell 导出 `GONGGA_MODEL`）会把测试 roster 放不下的
  默认模型塞给服务端，启动探针直接拒绝。`scripts/*_selftest.sh` 的每个服务端启动行
  都带；新写的验证脚本照抄。
- **`trap` 里 kill + `rm`**，否则下一次删不掉 `gongga.db`（busy）。
- **一行 verdict**：`PASS/FAIL · 耗时 · build sha`，别让下一个 agent 读日志自己判断。

**一个服务端生命周期跑完 N 个场景**：写成 `scenario | args | expected` 的表，
一次起服务端、跑完全表、一次收尾。进程生命周期类的场景（关窗口 → 服务端停）没法共用，
另开一个脚本，别混进去。

### 多 agent 共机的硬约束

- **每个 worktree 自己的 `CARGO_TARGET_DIR`**。共用一个 `target/` 时并发 cargo 排队等锁，
  而且 Windows 锁着正在跑的 `.exe`，会直接 `拒绝访问 (os error 5)`。
- **`.dev/` 会长到 GB 级**：`evolve.sh stage` 把构建放 `.dev/target`（为了绕开 exe 锁），
  历次 `*-home/` 各带一份 `ship` checkout。验证脚本自带清理，或别在这里攒。
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

### 别让程序替你杀

上面那一节讲的是**你**动手。真正难防的是另一种：你按下的键让别的进程去杀——
bundle 的 retire 会让旧 build 的服务端 `cancel`（除非那个程序自己不起服务端），
窗口退出时的 `cancel` 也一样。命令是你敲的，账记在你头上：那个 home 的服务端停了，
它在跑的 turn 一起没了。

判据还是「是不是我的 home」，只是提前一步：**跑任何会写 home 的程序之前，先确认
`--home` / `GONGGA_HOME` 指的是自己的**。指向一个在用的 home 不是只读——
见 `docs/notes/bundle-main.md` 的 "There is no read-only run"。

### 结果落成数据，不是笔记

`.dev/verify/<sha>-<area>.json`：build、命令、场景、结果、耗时。下一个 agent **先读**，
同一个 sha 命中就引用，只有 sha 或场景变了才付 L3 的钱。

这与「不要写交接笔记」不冲突：笔记是散文，会撒谎；这里每条的失效条件是写死的（sha 或
场景一变就重测）。**散文不写，测量要存。**

## 发布（改过 ship 内容之后）

**内容不再有 pin。** 二进制里只有内容仓库的 URL 和它读的 `protocol`；服务端把内容仓当成
`fork` 那种 checkout（官方 `main` + 本机 `local`，见 `docs/notes/server-fork.md`），从这个 checkout
里取「说它读的那个 protocol 的最新 commit」。所以发布 = **一次 push**，不产生本仓库的提交、
不重新编译。

```bash
scripts/ship.sh -m "heir: 把 mentor 写进指引"   # 提交 local → rebase 到 main → push 到 main
scripts/ship.sh --check                          # 两端 protocol 一致吗 + 推上去了吗
scripts/ship.sh --check --local                  # 同上，不问网络（pre-commit 用这个）
scripts/ship.sh --dry-run -m "..."               # 会做什么（不写不推）
```

- **`.gitmodules` 里 `ignore = all`**：内容仓的 commit 与本 build 无关，所以改 skill 不会在本仓库
  留下 `M ship` 这类半提交状态。别去掉它。
- 有改动时 `-m` 必填：脚本不替你编一条发布说明。发布前的 rebase 是内置的，冲突就整体撤销、
  什么都不推——自己手动解完再跑一次。
- `--check` 的退出码是契约：`0` 一致、`1` 内容说的 protocol 与本 build 读的不一致（真漂移）、`2` 用法、
  `3` **远端没应答**（"推上去没有"还没法判断）。第三种不是不一致，别当成不一致处理。
- **protocol 什么时候动**：只有内容的**形状**变了（新增强制字段、换布局）才在本仓库
  `crates/server/ship.toml` 里加一；一处 prose 修正、加一个 skill、退役一个路径都是内容，
  都不该碰它。动的时候一次只切一档：先发内容、再发读得懂它的服务端，老 build 会沿着
  first-parent 往回走，继续装它读得懂的那个 commit。
- 它自己有测试：`bash scripts/ship_selftest.sh`（`--keep` 留住现场）。整条流程跑在临时仓库 +
  临时 home + 真服务端上，不碰 GitHub。改内容仓解析这件事必须跑它——`--check` 通过不代表
  装上去了。

**提交时挡的是 protocol 漂移**：`scripts/hooks/pre-commit`，只在这次提交动了 `ship` 或
`crates/server/ship.toml` 时才跑，用 `--check --local`（提交不能依赖网络）。它挡不住
`--no-verify`、没装 hook 的 clone、以及任何不是这个 git 发的推送；那些只有接收侧能挡，
靠 `gongga-ship` 的 main 禁 force-push、禁删分支。每个 clone 装一次：

```bash
git config core.hooksPath scripts/hooks
```

## 改过服务端代码之后

跑着的进程还是旧二进制，新代码要接管这个 home 才算生效。整套流程在 `evolve` skill 里
（`skill` 不带名字会给出路径），**不在这里重述**：那份散文和 `evolve.sh` 一起版本化，写第二遍
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
| `harness` | `$GONGGA_HOME/skills/<name>` | 内容仓库 `ship`，按 protocol 取最新 commit | 改 `ship/skills/<name>` 再发版 |
| `general` | `~/.agents/skills/<name>` | 社区 clone `gongga-agents` | 改社区仓库 |

**别全读，也别 grep 目录**：调用 `skill` 工具（不带 `name`），它返回 name / description / path /
bucket 一张表，命中才按它给的路径读正文。列表本身不写在这里——写在这里就等于每加一个
skill 都要改一次这个文件，而读的人手上已经有那张表。

**需要源码的 skill 不属于 `ship`。** `evolve` 要往上找 `Cargo.toml`、要编服务端、要读
`.dev/`，装进 `$GONGGA_HOME` 之后它找不到仓库根，所以它住在**这个仓库**的
`.agents/skills/evolve/`，随 checkout 走，不随内容仓走。**不要往 `$GONGGA_HOME/skills/` 里放它的
副本**：那份副本会随源码一起变旧，而且它自己 `cd` 到 home 之后往上找不到 `Cargo.toml`，一行
都跑不通——但 `skill` 会把两个同名 `evolve` 都列出来，读的人先撞上坏的那个。

其余（`heir` / `image` / `self-control` / `subagent` + 默认 config）在内容仓库里，是这个仓库的
**submodule**（`ship/`）。改 skill 就是**在 `ship/` 里改然后发布**（`scripts/ship.sh -m`），
不产生本仓库的提交：内容仓的 commit 与本 build 无关（`ignore = all`），服务端按 protocol
自己取最新的那个。`ship/skills/<name>` 就是一层，没有 `harness/` 那一层——它原样映射成
`<home>/skills/<name>`。改 `$GONGGA_HOME/skills/` 里的已安装副本会被下一次合并判成
「读者自己写的」而不再更新。

**跑一个 skill 的脚本：从它自己的目录出发，别写桶的路径。** 正文里写入口用
`SKILL_DIR=<本 SKILL.md 所在目录>`（`@skill(...)` 展开的 `SKILL.md: <path>`、`skill` 的
`path` 列都给得出它），再 `"$SKILL_DIR/<entry>" …`：home 会搬、桶会变，而别的 harness 不认
`<home>/skills/...` 也不认 clone 路径——它读到的是一个不存在的命令。gongga 里更短的一层是
`gongga-cli skill <name> [args…]`（`skill ls` 是它的诊断动词），
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
  `@file(...)`/`@skill(...)` 会进 index，和空会话的首轮一样）；mentor 那边会补一条 tool_result 告它
  自己成了 mentor，只答被问的。
  之后卡住了用 `get_messages` 读 mentor、用 `send_message` 问 mentor。
  read 读的是消息（模型输出 + 注入），工具调用与 reasoning 被过滤掉。
