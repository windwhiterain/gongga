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

## 没做的，和为什么

- **社区 skill 的正文**（web-search / proxy / computer_use / model_pool / polity / latency /
  theme-cli）：那些文件在 `windwhiterain/gongga-agents`，不在本仓库。它们是这条契约最大的受益
  者（现在写的是 clone 内部路径），但改要发那边的 PR。
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

**没有动 `reconcile`。** 它的 Invariant（"A file is never overwritten while it differs from
the content that installed it"）不该我一个人改，两个候选方向留给用户：

- **A 冲突也记账**：Conflict 时把路径写进 `kept` 并让 pin 前进，base 立刻变新鲜，这个文件
  从此按"当前字节"参与下一次合并。代价：真冲突之后用户不再被提醒，而现在这条 halting
  就是"提醒"的实现。
- **B 认出身世**：冲突分支里也走 `is_a_past_install`（磁盘字节等于缓存里某个 commit 的同名
  blob → 安装产物而不是人改的 → 更新）。代价是每个冲突文件一次历史遍历，与"O(1)，至少
  O(log n)"的品味冲突，除非换成一次 hash 索引。

两者都要单独验证。这一轮已经换过一次手，不适合在同一轮再 stage 一次。

## 验证

`cargo test --workspace` 在这个仓库是**空的**（全 workspace 零个 `#[cfg(test)]`），所以 L1 没有
东西可跑；实际验的是编译 + `clippy`（新文件零警告）+ `comments-check`（新文件零违规）+
`test-layout-check` + `scripts/skill_contract.sh`，再加一次手动冒烟：

- `.sh` 入口：`$here` 走 POSIX 拼法解析正确（`dirname "${BASH_SOURCE[0]}"` 没坏）；
- argv 原样：`one "two words" --home x --flag=y` 五个参数原样到达，`--home` 没被 flag-order 拦；
- 退出码：入口 `exit 7` → 本进程 7；
- `.py` 入口：探测到 3.12；
- shadow：一个 home 里也有同名 skill 时，`ls` 打一行 `shadowed`，`which` 列出两条。
