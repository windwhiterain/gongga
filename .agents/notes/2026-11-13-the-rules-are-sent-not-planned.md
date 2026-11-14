# AGENTS.md 不进 prefix cache 的记忆模块

User's instruction, 2026-11-13:

> 这个 harness 的 agent 对 AGENTS.md 引用的 whiting-dev skill 遵守度极差，请把关于 worktree
> 一节的内容精简后放到 AGENTS.md 里，同时在 prefix cache plan 里把 AGENTS.md 单列到最外面，
> 提高它的优先级。

> 或者说，AGENTS.md 不参与 prefix cache 的记忆模块，始终注入在最前面，只是它引用的 meta
> reference 输入给 prefix cache plan 做 root。

所以这是两件事：

## 1. worktree 一节进 AGENTS.md

`whiting-dev` 的 `## Worktree` 五条（每个 session 一个 worktree、`./.worktrees`、一件 feature
一个分支、检查点合回 main、最终合并后删分支、子 session 的 worktree 由 parent 删）压成两行
放进 `AGENTS.md` 的 `## Worktree`。技能正文没动：它讲「怎么读、怎么测、什么时候停」，
worktree 只是其中一节，删掉会让那份文件不再自洽。

## 2. rules 文件自己不再是缓存树的一个 item

改之前：`meta::roots` 把 `@file(AGENTS.md)` 播种成**第一个 root**，它的正文作为 `wanted` 的
第一项交给 `prefix_cache::plan`。于是它和其它记忆模块一样：

- 价格搜索可以先挑一条不带着它的链，它就被排到后面；
- 编辑落在 `index_change_tolerance` 里时，头里留着**旧正文**，改动作为 patch 落到整个 index
  之后——模型读到的规则是旧的，新的在末尾；
- `expire` 可以把它连同整条链一起丢掉。

改之后：`meta::rules` 读它，`index_of` 把它**原样打印在 index 最前面**，不进 `wanted`；
`meta::roots` 只走它**引用的**那些东西（`@file(SPEC.md)`、`@skill(whiting-dev)`、
`@skill(clean-disk)`），并且排在消息命名的引用**之前**——同一目录的每个会话都共享这一段。

一处必须跟着改：`rooted`（`store::Prompt` 里那份「这次请求发出了哪些拼写」）不再含
`@file(AGENTS.md)`。它不只是文档，`Agent` 在 `LlmEvent::Completed` 用它去 `prefix_cache::confirm`
把链标热；链的第一项不再是它，留着就会在第一步就对不上，从此一次都不会被标热。
`scripts/remote_selftest.sh` 里那条断言因此从 `rooted` 改看 `index`。

## 观测

`scripts/meta_selftest.sh` 在合并后的树上（sha a089372d）打印的 `rooted`：

```
@file(SPEC.md)          <- AGENTS.md 自己引用的，现在排最前
@skill(whiting-dev)
@skill(clean-disk)
@file(docs/notes/tui-main.md)   <- 消息引用的
@skill(self-control)
...
```

`@file(AGENTS.md)` 不在其中，而它在 `index` 的开头（同一脚本的 row 段与 `remote_selftest.sh`
检查的是这件事）。`append_only_selftest.sh` 仍然 PASS：会话中途改 AGENTS.md 不动头，也不把新行
带进已发出的请求。

数据：`.dev/verify/d2a77de2-rules-not-planned.json`。

## 没证明的事

- 遵守度是否真的变好：prompt 的字节不是行为的证据，也没有为此起付钱的运行。
- 远端那条断言：本机 peer（`lichen-vm`）不应答，所以是读过的，不是跑过的。
