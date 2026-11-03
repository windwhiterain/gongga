# 会话名是路径，列表是树

日期：2026-11-02 · worktree `.worktrees/session-tree` · 分支 `feature/session-tree`

## 用户要的

> 让 session name 支持路径，例如 "a/b/c"；tui 要用 tree 的方式渲染（可展开折叠）。

## 批准的四点（用户「好」）

1. SPEC 新增 `## Session Name`，UI 那行补 tree 语义 —— 已写。
2. 折叠状态**只在内存**（像 `abstract` 的展开），不写 `cli.toml`、不加新命令。
3. group 行显示子孙**最强**状态（error > running > idle）。
4. 在 worktree `.worktrees/session-tree` 做，用户自己合并回 `main`。

## 改了什么

- `SPEC.md`：`## Session Name`；UI `[session{...}]` 那行加 `tree: ...`。
- `crates/server/src/session.rs`：`checked_name` —— `/` 分段，段不得为空。
- `crates/server/src/monitor.rs`：`reserved` 改成**逐段**检查 `@`（`work/@monitor` 也拒）。
- `crates/server/src/commands.rs`：`set_session` 先 `reserved` 再 `checked_name`。
- `crates/tui/src/tree.rs`（新）：`Row` / `Mark` / `rows()`，深度优先、显式栈。
- `crates/tui/src/panes.rs`：`list_names` → `list_rows`，加 `folded: BTreeSet`、
  `row_of`、`toggle_row`（点击时重建，不在 frame 里改形状）。
- `crates/tui/src/render.rs`：`draw_list` 画 `○   ▾ [api]`；`list_width` 按行算；
  `list_at` 返回 `ListHit { name, marker }`。
- `crates/tui/src/lib.rs`：`Part::Fold`；点 marker 折叠；点纯 group 的名字也折叠；
  纯 group 行的滚轮归列表。
- 文档：新 `docs/notes/tui-tree.md` + README 索引；同步 `tui-panes` / `tui-render` /
  `tui-lib` / `server-commands` / `server-session`。
- `scripts/tui_live_scenarios.py`：新场景 `session-tree`；顺带把四个场景开头
  `expect model fake` 改成 `expect session(s)`（见下）。

## 撤销的一个改动（记下来，别再来一遍）

曾经把 `tools/bash.rs` 的 `slug` 提到 `tools/mod.rs`，并让 `tools/detached.rs` 的
日志名 `logs/{session}-{job}.log` 走 `slug`，理由是我以为 `a/b/c` 会让 detached bash
起不来。**这是错的**：`DetachedJob::launch` 自己 `create_dir_all(log.parent())`，所以
`logs/a/b/c-1.log` 会被踏实创建出来。而且 `slug` 把 `a/b/c` 和 `a-b/c` 压成同一个名字，
比原来的嵌套目录更差（日志没有被任何目录扫描依赖，只从数据库行读回）。整段已回退。

## 验证

- `cargo build --workspace`、`cargo fmt --check`：干净。
- `comments-check`：与 `main` 的违规清单逐文件计数**完全一致**（33 条基线违规都是旧的），
  本次新增的注释零违规。
- `test-layout-check`：0 violations。
- 真帧（`python scripts/tui_live.py --scenario session-tree`，138 帧 / 46s，PASS）：
  四层断言全过 —— 树形（`a`/`b`/`[c]` 三层）、点 marker 折叠、点纯 group 的名字折叠、
  点名字仍然 show/hide 窗口、`a//b` 被拒并说明哪段为空。最后一帧：

```
○ ▾ a
○   ▾ b
○       [c]
○ ▾ lab
○     [x]
○ ▾ [work]
○     [web]
○     [api]
```

## 发现的旧问题（不是这次改动引入的，留给用户）

1. **四个 tui_live 场景的开头 `expect model fake` 早就不会出现了**：7615abe
   "the header drops the model" 把 model 从 header 拿掉，场景没跟着改，所以
   `single-stream` / `two-sessions` / `event-ui-toggles` 都在第 2 行卡住。已改成
   `expect session(s)`（唯一改动），但那三个场景里还有 `--mode line/none/full`，
   但现在 `set_render_session` 收的是 `--show <bool>`，跑到那一步仍会失败。
2. **这些场景会花真钱**：假端点只接管"服务端自己的模型"（`GONGGA_BASE_URL`/`GONGGA_MODEL`
   进的是 `cfg.model`），而 `set_session` 不写 `--model` 时用的是常量
   `DEFAULT_SESSION_MODEL = deepseek-official/deepseek-flash`，于是会话的回合直接打真 provider。
   本轮我踩了一次（`.dev/live/20261102-235156`：fake 端零请求，真模型在我的仓库里跑了几条
   bash 工具）。所以 `session-tree` 场景现在**不发消息**，只建会话、只看树。要修的话：
   tui_live 加一个只含假 provider 的 catalog（`GONGGA_PROVIDERS_CATALOG`），场景一律
   `set_session --model fake/...`。

## 没做的决定（留给用户）

- 兄弟节点排序沿用 announcement 顺序（新会话在前），没有按名字排序。
- 折叠状态不持久化：重启客户端全部展开。

## 后续一：marker 缩成一列（用户点评）

用户看过真帧后说：「你用的符号太占空间了，用 `|`，一个栏目折叠时显示 `|a`，展开时显示 `a`」。

原来每行是 `▾ `/`▸ `/两个空格` 的**两列** twisty，每行多花一列，而且 `▾` 在重复 indent
已经说过的父子关系。现在：

- marker **恒为一列**：折叠 `|`，其余（展开 / 叶子）一个空格。空格不是「没有列」——
  有了它，`|lab` 和旁边的 `[work]` 标签才落在同一列；没有它，只有折叠的那一行会往右挤一格。
- 标签位置：`2 + 2*depth + 1`（状态列 + 空格，indent，marker 列）。缩进仍是每层 2 列。
- 点击：`Part::Fold` 只认 marker 那**一列**（`cell == marker_column`），名字照旧。
  会话+分组行两种手势都还在：名字 show/hide，marker 折叠。

真帧（`--scenario session-tree`，129 帧 / 46s）：

```
○  a
○    b
○      [c]
○  lab
○    [x]
○  [work]
○    [web]
○    [api]
```
折叠时 `○ |lab` / `○ |[work]`，展开回到 `○  lab` / `○  [work]`，标签列不动。

注意：一个「既是会话又是分组」的行展开时 marker 是空格，看起来和叶子行一样——差别只在
它下面有没有缩进的行。这是用户要的形态（折叠才有符号），折叠仍然可达（点 marker 列）。

## 后续二：展开的行不再留空列（用户第二次点评）

用户：「不显示 `|` 的时候要不要显示空格啊，空间宝贵」。对——展开行本来就占着那一列，
那就不是「预留」而是「白付」。现在：

- marker 只有折叠时存在：`○ |lab`；展开时没有这一列：`○ lab`，标签左移一格。
- 列表宽度按行算，所以**展开时窄一列**（真帧里 panes 那边多出 2 个字符），
  折叠的那一行暂时宽一列；若最宽的行恰好被折叠，整个列表会临时宽 1 列。
- indent 不动：每层仍是 2 列空格（结构，不是符号）。

代价与补法：留空列时它还兼着「展开状态下的折叠按钮」，去掉之后，展开的**会话+分组**行
（如 `work` 自己也是会话）就没地方折了——名字被 show/hide 占着，`|` 只在折叠时才有。
于是让**状态标记列**（列 0，每行都有、不占额外宽、且不是文字）也能折叠：

| 点击位置 | 会话行 | 纯分组行 |
| --- | --- | --- |
| 名字 | show/hide 窗口 | 折叠 |
| 状态标记 / `|` | 折叠（有子行时） | 折叠 |

`render::ListHit` 的字段从 `marker` 改名 `fold`，两个落点合成一个判断。

真帧（`--scenario session-tree`，141 帧 / 44s）：树形、`[work]` 点状态标记折叠、
点 `|` 展开、`lab` 点名字折叠/展开、`[lab/x]` 点名字 show/hide 全过。

**待用户定**：状态标记折叠这个手势要不要写进 SPEC（现在 SPEC 只写「a click on its
marker folds it away」）；不想要的话，替代方案是只给会话+分组行留那一列（特例），
或者干脆接受那种行只能靠 `|` 展开不能折。

## 后续三：让 subagent / heir 用路径化命名（用户选 B）

用户：「接下来要让 subagent, heir 等等都用上路径化的命名（不能让覆盖这种事情发生）」，
选项给了 A（只改技能）和 B（再给 `set_session` 加硬保证），选 **B**。

### 硬保证：`set_session --must_create <bool>`

- `crates/server/src/commands.rs`：catalog 加 `must_create` 参数；`set_session` 在
  `reserved` / `checked_name` 之后、解析 model 之前检查——**存在即拒**，且什么也没写。
- `SPEC.md`：`set_session` 那行加 `[--must_create <bool>](initial: false)` 与一句理由。
- 为什么必须有它：`set_session` 的语义是「不存在就建、存在就选中」，两种情况都答
  `session is now X`。所以「我要开自己的会话」的调用方一旦撞名，得到的是**别人的
  transcript**，下一条 `send_message` 就把 brief 注进了别人的回合。探针 + 创建的写法
  有窗口期，输的一方正好是静默加入的那一方。

### 命名规则（两个技能都改成路径）

```
<父会话>/<角色>          例：main/impl
<父会话>/<角色>/<n>      已被占：main/impl/2
```

跨父会话结构上不可能撞（路径第一个段就不同）；同父重名加段。名字即会话列表树里的位置。

- `ship/skills/subagent/spawn`（新入口，`spawn.py`）：`spawn <role> [--cwd] [--model]
  [--context-limit] [--parent]`，父默认 `$GONGGA_SESSION`。循环 `set_session
  --must_create true`，撞了就把段往上加；stdout 只有最终名字一行（好写 `$(…)`），
  stderr 说跳过了哪些、抽了什么模型。
- `ship/skills/subagent/run`（新入口）：`metadata.entry` 从 `wait_turn` 改成 `run`，
  这样 `gongga-cli skill subagent spawn …` 和 `… wait_turn …` 都能到；
  `skill subagent wait_turn sub1` 的旧拼写照旧可用。
- `ship/skills/subagent/SKILL.md`：删掉写死的 `sub1` 例子，正文改讲路径名 + `spawn`，
  规矩里加「不要手写会话名」。
- `ship/skills/heir/transcript.py`：`--session` 默认 `$GONGGA_SESSION`；`--mentor` 默认
  `<你的会话>/mentor`；新增 `established()` / `free_name()`——**已被占的 mentor 不再追加**，
  改用 `<mentor>/2`。理由：继承人用 `get_messages --range 1:` 读回全部原文，两代档案
  串在一个会话里就分不出哪段是自己的过去。
- `ship/skills/heir/SKILL.md`：`mentor-<name>` / `mentor-main` 全部改成路径写法。

### 改了 `test_heir.py` 的三条断言（要报告）

它们断言的是**旧契约**（「第二次 handover 续进同一个 mentor」），与新契约正好相反，
所以按新契约改了，没有新增测试函数：`test_guards`（`--mentor <self>` 现在落到 `<self>/2`）、
`test_heir_moves_every_row_then_empties`（第二份档案是 `mentor-main/2`，旧档案一行不动）、
`test_a_bad_copy_rolls_everything_back`（篡改要打在脚本真正会用的 `m1/2` 上）。
`python test_heir.py`：7 条全 ok。

### 验证

- `cargo build --workspace`、`cargo fmt --check`、`test-layout-check`：干净；
  `commands.rs` 的 comments-check 仍是 10 条旧违规，没新增。
- `bash scripts/skill_contract.sh`：PASS 8 skill(s)。
- 真进程（`.dev/spawn-e2e.sh`，跑完即删；临时 home + 内核分配端口 + pid 从 `server.lock` 读）：
  12/12 PASS —— `main/impl` → `main/impl/2` → `main/impl/3`、`main/impl/review` →
  `main/impl/review/2`、`--must_create` 拒已存在且不改动、`fresh/one` 能建、
  不带 `--must_create` 仍能选中、`a//b` 仍被文法拒、`spawn a/b` 拒多段角色。

### heir 的真进程验证（`.dev/heir-e2e.sh`，跑完即删）

真服务端 + 真 db（用 sqlite 直接写 seed 行，因为验证 turn 需要 provider）：

- `transcript heir --note …`，**不给** `--session` / `--mentor`：`$GONGGA_SESSION=main`，
  档案落到 `main/mentor`。
- 第二次 handover：档案落到 `main/mentor/2`，`main/mentor` 一行不多。
- 两代内容不串：`改服务端` 只出现在 `main/mentor`，`main/mentor/2` 只有第二代。
- 12 条里 4 条（spawn 那 12 条另算）全 PASS。

踩到的两个小坑，记下来免得下次重付：`messages.content` 是 JSON，��文在里面是 `\uXXXX`，
按原文子串搜会假失败（要 `json.loads` 再比）；`send_message` 发的第一课会被服务端写回
learner，所以第二次 handover 的档案行数不等于「我插了几行」。

## 后续四：heir 去掉 `--session`（用户指出）

用户直接问：「heir 脚本不是可以读环境变量获得调用者的 session name 吗，为什么要给参数？」

对。查过三件事：

1. `GONGGA_SESSION` 由 `session.rs::session_env` 注入给**每个会话的每个本地 shell**，
   detached 作业也有（`detached.rs:341`）。
2. 远端 shell **拿不到**它（`shell_set.rs`：本地走 `BashShell::spawn(..., &self.env)`，
   远端走 `RemoteShell::new(endpoint, cwd, door)`，env 丢了），`GONGGA_HOME` 同样丢，
   所以 heir 在远端 shell 里本来就跑不起来。
3. 于是 `--session` 的唯一实际作用是让调用方**指定别人**——实测 `heir --session @monitor`
   真删掉了 harness 收件箱的 2 行。参数本身就是洞。

改法（不是加检查，是把参数删掉）：

- `read` / `heir` 都没有 `--session` 了；`GONGGA_SESSION` 取不到就拒绝。
- `--mentor` 保留（它命名的是**档案**，不是被读的 transcript），但**必须**在
  `<caller>/` 之下。结构性规则，不需要知道哪些名字保留：`--mentor @monitor`、
  `--mentor peer`、`--mentor meh`、`--mentor main`（本身）全拒；
  而 `@monitor` 培养继承人时 `@monitor/mentor` 合法——它是自己的子节点。
- `cmd_heir` 里"mentor 不能是自己"的那条死检查删了（现在不可能）。

验证：

- `python ship/skills/heir/test_heir.py`：7 条全绿。`run()` 改成注入 `GONGGA_SESSION`，
  `test_guards` 现在断言两条新拒绝（缺变量、mentor 在子树外），换掉了原来那条
  "mentor 与自身同名"的断言。
- **L2 变异**：撤掉子树检查 → 1 条红；撤掉缺变量检查 → 1 条红；恢复 → 0 条。两条门都真的守着东西。
- **真进程**（装好的那份 `$HOME/skills/heir/transcript`，`GONGGA_KEY` 显式传、
  临时 home、内核分配端口）：7/7 PASS —— `--session` 变 unrecognized、缺变量被拒、
  三个越界 mentor 全拒且 @monitor/peer 一行未少、默认落 `main/mentor`、
  `@monitor` 落 `@monitor/mentor`。
- 已发版：`ship` `0c127db` → pin `0c127db`（`ship.sh --check` exit=0），本仓库 `0b46d51` 已 push。
