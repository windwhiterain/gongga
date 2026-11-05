# 引用是一个组：`@file(path)`，不是"冒号加一个词"

2026-11-05。用户拍板：**硬切**，只认 `@kind(值)`，老的 `@kind: 值` 不再解析；括号按真正的
括号解析（嵌套计数、引号里的不算）。

## 为什么切

先是试了"不改语法、把结尾交给机器判断"：`@kind:` 之后的 run 取到空白为止，结尾用盘上
`stat` 的最长可解析前缀定（编辑器识别链接就是这么做的），skill 用已安装名字的最长前缀。
它在本地跑得通（`看下@file:src/meta.rs这个文件` 认得出 `src/meta.rs`），但那是**推断**：

- 答案取决于盘上此刻有什么，不取决于文本。同名文件出现/消失会改变一次解析的结果。
- 远端会话要么每个候选付一次往返，要么就得承认"本机和 peer 的规则不同"。
- 中文路径 + 中文散文（`文档/说明.md这个文件`）只有 `stat` 分得开；纯文本规则分不开。

分隔符把这个自由度彻底拿掉：值在配对的那个 `)` 处结束。`@` 不必在词首，值不必停空格上，
值里可以有空格和括号。**歧义不是被解决，是不存在了。**

## 现在的语法

- 前缀是 `@image(` / `@skill(` / `@file(`——**`(` 是拼写的一部分**，所以 `@file` 单独出现就是
  文字（`@monitor` 因此永远不是引用）。
- `group()` 数括号、跳过引号内的一对（`@file(a(b).md)` → `a(b).md`）；没有配对的 `)` 就不是
  引用（人还在打字）。
- 值两侧的空白剥掉，再剥一层成对的引号（值本身以空格开头/结尾时才需要）。
- `expand` 写回的形状跟着变：`@skill(name) (SKILL.md 的路径)`、`@image(path) (copy id)`；
  index 的 key 是 `@file(path)`。

## 改到哪儿

- `protocol::Reference::prefix()` 现在是 `@file(`；`name()` 是新加的（`file`）。
- `meta::mentions` 回到**纯函数**：不需要 `Source`，不需要 `stat`，`Source::holds` 删掉了。
- 补全：前缀插 `@file(`（无尾随空格），值插 `path)`（目录除外——`Tab` 继续往下走）；
  `Offered::start` 把替换范围的起点交给 popup（`@` 可能在词中间，`看下@file(x)`）。
  这一条也是本轮唯一抓到的 popup bug：第一版把值从 `@` 开始替换，`@skill(sel` + Tab 把
  `@skill(` 一起吃掉。
- 文字：SPEC 的 Meta 段、AGENTS.md、DESIGN.md、`docs/notes/*`、`scripts/*_selftest.sh` 的
  断言与消息、`.agents/skills/evolve`，以及 `ship/` 内容（9 个文件 54 处，含 `image/show.py`
  里"路径不能有空格"那段——现在有空格反而是合法的，改成拒绝**括号不配对**的路径）。

## 证据

- `scripts/meta_selftest.sh`（真 server + fake LLM + 临时 home）：一条消息里同时有
  `看下@file(docs/notes/tui-main.md)这个文件`、`@skill(self-control)是做什么的`、
  `@file(…\a note.md)`、`@file(…\a (1) b.md)`；index 与行都按四个完整值落地，
  `@skill` 那处展开成 `@skill(self-control) (…SKILL.md)是做什么的`。**PASS · 15s**。
- `scripts/tui_live.py --scenario reference-in-word`（真窗口、ConPTY）：`看下@fi`+Tab →
  `看下@file(`；补完路径 → `看下@file(docs/notes/tui-main.md)`；`用@sk`+Tab → `用@skill(`，
  再补 → `用@skill(self-control)`。**exit=0**。
- `cargo build --workspace`、`cargo fmt --check`、`comments-check`、`test-layout-check`、
  `scripts/skill_contract.sh`、`cargo test --workspace` 全绿；`scripts/ship.sh --check` = 0。

## 落地

`feature/meta-reference`（4 个提交）已在 `main` 上（`4d94099`，fast-forward，没有 merge commit
之外的改写）。之后三件按顺序：`evolve` 让新 build 接管这个 home —— 这一步会 retire 旧 build，
正在跑的 turn 会随之结束，所以要用户点头；然后 `scripts/ship.sh -m "…"` 把内容推上去。

## 还没做的一件

`ship/` 的内容已经在新 clone 上提交成 `local`（`references are groups: …`），但**没有 push**：
现在 push 就是让还在跑旧 build 的服务端读到教新拼法的散文，那段时间里 `image`/`show` 打出来的
`@image(...)` 会被旧解析器静默丢掉。顺序是先让新 build 接管 home（`evolve`），再
`scripts/ship.sh -m "…"` 把内容推上去。协议号不动（内容形状没变，只是散文与拼法）。
