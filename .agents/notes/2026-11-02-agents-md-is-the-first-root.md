# cwd 的 AGENTS.md 成为 index 的第一个 root

User：「现在没有把 session cwd 下的 AGENTS.md 自动发现并装入首轮 prompt 的 cache opt，加上这个功能」。

## 做了什么

`meta::RULES = "AGENTS.md"`。`meta::roots` 的队列现在**从这条种子开始**（`@file:AGENTS.md`），
再接消息里命名的引用。于是它走的是别人早就有的那条路：同一个 `content()` 读文件、同一个
`spelling()` 当 key、同一个递归 walk（所以 AGENTS.md 里写的 `@file:notes.md` 也会进 index）、
同一个 `prefix_cache::plan` 排序、同一个 `prompt_in_force` 比对后写行。

放进 index 而不是拼进 `instructions`，是这件事的一半：index 在 transcript 之前，随前缀缓存
只收一次费；拼进消息则会跟着 transcript 每轮重发。另一半点：它同时是一个 spelling，所以消息
里写 `@file: AGENTS.md` 会留在字面、指向同一条 index 条目，一份文件不会被带两遍。

只读**会话 cwd 里的那一个**：父目录不继承，`~/.agents/AGENTS.md` 不读（那是 2026-10-28 的
决定，改为 skill `whiting-dev`，仍然有效）。

## 为什么这不是 2026-10-28 那次的回退

那次删的是 `prompts::constitution`：把两个文件直接拼进 `instructions`，每轮渲染、没有记录、
不管这一轮用不用得上。现在回来的是：

- 一个文件，位置是会话站的地方；
- 走 index，和别的 root 一起被排序、一起存进行里，改了是**追加一行**而不是悄悄改已发内容；
- 因此它和 skill 那套的不冲突处也保住了——机器自己的常驻规矩仍在 `~/.agents/skills/`。

`ship/skills/self-control/SKILL.md` 里「服务端不读任何 AGENTS.md」的说法一并改掉。

## 验证

`bash .dev/agents_md_selftest.sh`（一个服务端、两个会话、一次收尾）：

| 检查 | 结果 |
| --- | --- |
| 两轮只写一行 prompt | ok（一轮 1 行） |
| index 里有 cwd 的 AGENTS.md | ok |
| index 里有它命名的 `notes.md`（递归） | ok |
| `rooted` = `["@file:AGENTS.md", "@file:notes.md"]` | ok |
| 规则只在 index 里、不在消息里 | ok |
| cwd 没有 AGENTS.md 的会话不写 index | ok（`index: null`） |
| 第二轮吃缓存 | ok 1823/1843 = 98.9%（第一轮首个请求 189/1823） |

`comments-check` 156 处（改动前后都是 156，没新增；156 里有别人提交带来的），
`test-layout-check` 0，`cargo fmt --check` 干净，`cargo test --workspace` 这个检出里没有测试
（空跑，行为没动）。

`usage` 事件按 step 记，所以「第一轮的第一个请求」和「第二轮的第一个请求」是可分的；用
`sessions.context_tokens` 会把一轮里最后一步当成整轮，那一轮模型探索了 5 步的话读数是 11.7k
而不是 1.8k。

两点实测得到的教训：

- **fixture 里不能有指令**。第一版 AGENTS.md 写「每个回答都以 ORANGE 结尾」，模型就真的用
  bash 去改 `notes.md`，于是第二轮 index 变了、多写一行——那是正确行为，是测试设计错了。
- `target/debug` 被别的会话重建过，导致跑到的是**没有这个功能**的二进制（`AGENTS.md` 字符串
  在 exe 里 count=0）。现在验证脚本用 `.dev/target-amd` 自己的 `CARGO_TARGET_DIR`。

## 还没做

- SPEC 的 `### Meta Prefix Cache Optimize` 还只写「消息里命名的引用」，没写 cwd 的
  AGENTS.md 是第 0 个 root。SPEC 要 user 许可才能动。
- `cd` 到另一个有 AGENTS.md 的目录之后的行为没单独测：代码上 index 每轮按 shell 报的目录
  重建，所以那一轮会换文件并追加一行；这条只按机制推导，没实测。
- 本机 `~/.gongga` 仍跑旧 pin（`73e4a6a`）的散文与旧二进制，要 `evolve` 才换成
  `5510829` 的文案。
