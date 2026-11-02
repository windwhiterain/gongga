# 一个 skill 的入口只声明一次，正文从它自己的目录出发

2026-11-02。用户问：一切皆 skill 之后，怎样让 agent 调这些脚本不用每次列路径；skill 能不能
做成 package manager + namespace。查了生态之后答案分两半，记在这里以免下一个 agent 重新推导。

## 生态查到了什么（决定了形状，不是偏好）

- 规范（agentskills.io，Anthropic 维护，spec 页 `dateModified` 2026-08-04）定义的 frontmatter
  只有 `name` / `description` / `license` / `compatibility` / `metadata` / `allowed-tools`。
  **没有任何执行机制**，没有 `entry`、没有 `bin/`、没有 PATH 这一说。
- 脚本怎么调，规范给的是「相对 skill 目录，由 agent 解析」：
  *"Use relative paths from the skill directory root … The agent resolves these paths
  automatically."*
- PATH 注入在生态里只有一个来源，而且是**插件**不是 skill：Claude Code v2.1.91+ 对 plugin 的
  `bin/` 做 auto-PATH，microsoft/apm 因此把插件可执行文件部署到那儿（`apm@216bec6`）。
  Codex / Cursor / Gemini 没有。
- `*_SKILL_DIR` 环境变量在任何宿主上都不存在；只有 Claude 有 `${CLAUDE_SKILL_DIR}` 模板替换。
  EveryInc 三家实测的结论写在他们 `bundled-script-path-resolution-across-harnesses.md`
  里，并且明确说了「vendor docs alone do not settle a cross-harness question」。
- 命名空间在生态里落在**安装器**一侧：`apm install owner/repo --skill x`（manifest +
  lockfile）、`npx skills add owner/repo`、ClawHub 的 `@owner/slug`（保留裸 slug 兼容）、
  Chainguard 的 OCI ref + digest（共享副本写 `.agents/skills/`，再给各 agent 建符号链接）。
  harness 内部「bucket:name」这种限定名**没有先例**；规范对撞名只要求一句 shadow 警告。
- 反方向的声音：arXiv 2608.12610 那套协议立的规矩是「纯文件系统，没有 manifest、没有
  lockfile、没有数据库」。与 apm 那一派在打架——所以「多源 lockfile」这次没有做。

推论：**方案 A（给每个 skill 往 `<home>/bin` 落一个同名 shim）被否。** 它把名字塞进 shell 的
命名空间（skill 叫 `git` 就顶掉 `git`），造一套生态里没有的 `bin/` 约定，而且别的 harness 上
是一个不存在的命令——比「路径写死」更糟，模型不会报错，它会猜。

## 做成了什么

1. **契约（正文）**：入口一律写成 `SKILL_DIR=<本 SKILL.md 所在目录>` 再 `"$SKILL_DIR/<entry>"`
   ——EveryInc 的 Tier 3 锚点，三家实测都过的那个写法。gongga 里这个值免费：`@skill:` 展开的
   `SKILL.md: <path>` 和 `index_skill` 的 `path` 列都给出它。
2. **契约（机器）**：`metadata.entry: <file>`。`metadata` 是规范里「任意 key-value」的扩展位，
   严格校验器（`skills-ref validate`）只会看到「多了一些 metadata」，不会报未知字段。
3. **便利**：`gongga-cli skill <name> [args…]`，加 `skill ls` / `skill which <name>` /
   `skill run <name>`（最后一个是给名字恰好叫 `ls` 的 skill 的逃生口）。链外执行：不需要
   runtime、不需要连服务端，参数原样透传，退出码就是入口的。`skill` 不是工具——它的价值就是
   不占每个请求的工具描述，所以也不该变成一个工具。
4. **一份解析**：`crates/skill`（新 crate，无依赖）。之前 server 的 `installed_for` 和 cli 的
   `installed_skills` 各自走一遍三根，注释各自声称「和另一处同序」；`skill` 一来就是第三份。
   bucket 词、优先级、frontmatter、`shell_path`（原 `server::tools::bash::msys_path`）都搬进去。
5. **门禁**：`scripts/skill_contract.sh` 拦两种走样——正文出现桶路径（`$GONGGA_HOME/skills/`、
   `gongga-agents/skills/`），`metadata.entry` 指不到文件。挂在 `scripts/hooks/pre-commit` 上，
   只在提交动了 `ship` 或 `.agents/skills/` 时跑（`ship` 是 submodule，改动只显示成这一条路径）。

## 三层的落地情况

同一份契约要在三个地方各落一次，因为 skill 分散在三层：

| 层 | 谁改 | 现在 |
| --- | --- | --- |
| project（`<cwd>/.agents/skills`） | 这个仓库 | `evolve` 已改 |
| harness（`<home>/skills`，pin 下来） | 本仓库的 `ship/` 子模块 | `heir`/`image`/`remote`/`subagent`/`self-control` 已改，pin 已发（`a198609`）|
| general（`~/.agents/skills`，社区 clone） | `windwhiterain/gongga-agents` | 9 个 skill 已改并 push 到 main（`efda7bb`）|

社区那层这次改了 9 个：`web-search` `proxy` `computer_use` `latency` `model_pool` `polity`
`theme-cli` `clean-disk` `windows-dev`。两处不是"路径替换"而是改写：

- **`clean-disk` / `windows-dev`** 用的是 `powershell -File "$HOME\.agents\gongga-agents\skills\<n>\<n>.ps1"`。
  改成 `-File "$SKILL_DIR/<n>.ps1"`，并在正文里点明 **`$SKILL_DIR` 是 bash 展开的，不是
  PowerShell 变量** —— 那一条不写清楚，读者会以为脚本里读不到它。
- **`model_pool`** 原来写的是 `~/.agents/gongga-agents/skills/model_pool/model_pool`，即 clone
  的路径，不是 skill 的路径：clone 换个名字、换个 checkout 就断。

## 跨层引用：一个 skill 的名字装在另一个桶里

`ship/skills/subagent` 要调 `model_pool`，而后者在 general 层。**本 skill 算不出它的目录**
（那正是"别写装在哪"的另一面），所以这里的答案是改用名字：

```bash
G=gongga-cli
S="$G skill model_pool"      # gongga 里的快捷方式
```

正文同时说明没有这个快捷方式的 harness 怎么走：读 `@skill: model_pool`，按它自己正文的
`SKILL_DIR` 跑它的入口。**跨层调用是"用名字"的正当理由之一**——不是偷懒。

## 门禁补了一种写法

`scripts/skill_contract.sh` 原来只拦 `$GONGGA_HOME/skills/` 和 `gongga-agents/skills/`。
这一轮真正修掉的一类路径它一个都没拦（`~/.agents/skills/<name>/<file>`），所以补上，并
要求**两段都是名字形状**（`[A-Za-z0-9._-]+`）：正文可以*描述*一个桶
（`~/.agents/skills/<name>/`，含 `<` `>`），但不能*按那个路径调用*。`<` 与全角句号因此不匹配，
而 `~/.agents/skills/model_pool/model_pool` 匹配。

## 没做的，和为什么

- **社区 skill 的正文**：已在这一轮改完并 push（见上一节），不再列在"没做"里。
- **`bucket:name` 限定语法**：没有先例，且会把一份优先级解析变成一份优先级解析 + 一份语法。
  做到 `skill which`/`skill ls` 的 shadow 警告为止——名字解析保持裸名 + 优先级。
- **多源 lockfile**（`skill add <git-url>` / `skill update <源>`）：现在的 pin 结构够用，且
  生态正在「纯文件系统」和「manifest + lockfile」之间打架。等有第二个源再动。
- **SPEC.md 的改动**：需要用户点头。要改三处 —— `Crate Structure` 加 `skill`；`Meta`/`Skill`
  段写明「skill 可声明入口，正文从它自己的目录出发」；`Cmd` 加 `skill` 动词。等批准。

## 落地时撞到的一个旧 bug：这个 home 早就收不到内容了

换手之后 `skill ls` 里 heir/remote/subagent 的门是 `-`（没有入口），`~/.gongga/skills/` 里
那几个 SKILL.md 还是旧正文。日志说明了原因：

    pin=b6493bac2a written=0 updated=1 merged=2 removed=0
      conflicts=[skills/heir/SKILL.md, skills/heir/test_heir.py, skills/remote/SKILL.md,
                 skills/remote/remote.py, skills/subagent/SKILL.md]

`reconcile` 是三方合并：base = `.ship-pin` 记的那个 pin，ours = 磁盘，theirs = 新 pin。
这个 home 的 `.ship-pin` 停在 `2e11a077`，而磁盘上的文件是**好几个更新的 pin** 装进去的
（`subagent/SKILL.md` 逐字节等于 c7e9e64，`remote/*` 等于 a6b5c3ef，`heir/SKILL.md` 等于
2b2061d0）。base 一旦落后于磁盘，每个改动过的文件都是「ours≠base 且 ours≠theirs」→
Conflict；而**只要还有一个 Conflict，pin 就不前进**。于是 base 永远停在那个旧 pin，下一次
启动把所有改动过的文件再判一遍——一个棘轮：第一次冲突之后这个 home 再也收不到内容更新，
而且日志里每次都是同一串 conflicts，看起来像"用户改过这些文件"。

这次的处理：把那五个文件按 pin 的 **blob 字节**写回 home。注意是 blob 字节，不是 `ship/`
工作树的字节——工作树是 CRLF，装进去的是 LF，差一个字节就再冲突一次。写回之后四个入口
都在，`skill heir/remote/image/subagent --help` 四条都跑通。

**用户选了 B，已实现。** 判据是"磁盘上的字节是不是某个 commit 的同名 blob"：是 → 这是
别的 pin 装进去的，不是编辑 → 上游覆盖它。落地时比原提案多做了半步：

- 光在 Conflict 分支里加判断不够。同一个陈旧 base 也能让三方合并**成功**——把两个不同 pin
  的内容拼成一段谁都没写过的正文，然后一声不响地写下去。所以那道判据放在 `decide` **之前**，
  三个"两边都动过"的分支一起管：`Keep | Merge(_) | Conflict`，命中就改成 `Update`。
- 只在 `!settled` 时问：已经被记成"读者的"文件不再重判。
- 三个分支之外不问，所以一次什么都不改的启动**不多花一次** `git rev-list`。
- `Report` 多一个 `rebased: Vec<String>`，日志里跟着 `conflicts=` 一起打印：修好一个 home
  这件事要让人读到一句话，而不是从一个计数里推出来。
- 原 `Decision::Keep if old.is_none() && is_a_past_install(...)` 那条特例被吸收了：同一个问题
  现在只有一个地方问。

放弃了什么：读者把某段正文逐字节改回旧版本，而同一文件上游也动过——那次改动会被上游覆盖。
那些字节除了"这个文件的某个更早版本"之外不携带任何本地信息，而 base 本来会说同一句话。

## 一处更正：kept 名单实际管的是什么

上一轮我说过「记成 kept 的文件，上游以后的改动不会再落地」——**错的**，实测推翻：本轮
换手时 `config/config.toml` 既在 kept 名单上、又被上游改过，结果是 `merged=1`，上游那一行
落进了 home（与 pin 的差异只剩行尾一个空行，来自本地那份）。

真正的作用只有一条，而且在 `reinstalled` 那一步：**被记成 kept 的路径，不会被当作
「别的 pin 装进去的字节」而覆盖**。它照走三方合并，也照样可能冲突（并因此冻住 pin）。

原先那行注释写的是 "A file recorded as the reader's is not offered the merge at all"，与代码
不符（`_ if settled` 排在 `Merge`/`Conflict` 之后，只接得住 `Keep`）。注释已改成实话，语义
没动 —— 要不要让 kept 真的"完全不碰"，是另一件事，得单独定。

**验证**：`scripts/install_selftest.sh`（新写的 L3，一个服务端生命周期一个场景，临时 home，
`trap` 收尾，一行 verdict）。三个场景 + 十条断言，18~30 秒：

| 场景 | 修前 | 修后 |
| --- | --- | --- |
| 陈旧 base + 磁盘是别的 pin 装的文件（合并本会成功） | 合并成立、文件被拼成两版正文 | `rebased=[…] conflicts=[]`，文件等于 pin，pin 前进 |
| 同一状态，第二次启动 | — | `written=0 updated=0 … rebased=[]`，pin 不动 |
| 读者真的改了一行 | 冲突、pin 不动 | 一样：`conflicts=[…]`、pin 不动、那行还在 |

把修复撤掉重跑（L2）：4 条断言变红，其中一条是"文件等于 pin"——它抓的正是那个不声不响的
拼接。恢复后 10 条全绿。

## 验证

`cargo test --workspace` 在这个仓库是**空的**（全 workspace 零个 `#[cfg(test)]`），所以 L1 没有
东西可跑；实际验的是编译 + `clippy`（新文件零警告）+ `comments-check`（新文件零违规）+
`test-layout-check` + `scripts/skill_contract.sh`，再加一次手动冒烟：

- `.sh` 入口：`$here` 走 POSIX 拼法解析正确（`dirname "${BASH_SOURCE[0]}"` 没坏）；
- argv 原样：`one "two words" --home x --flag=y` 五个参数原样到达，`--home` 没被 flag-order 拦；
- 退出码：入口 `exit 7` → 本进程 7；
- `.py` 入口：探测到 3.12；
- shadow：一个 home 里也有同名 skill 时，`ls` 打一行 `shadowed`，`which` 列出两条。
