# 2026-11-15 a skill is named by its name, and a path still resolves

Task: `run_skill` 的 `id` 目前是 `<bucket>/<name>`（`general/polity`、`harness/handoff`、
`project/evolve`）。用户要求：bucket 这些"超目录"去掉，就是 skill 名字；未来可能有子目录
的 skill，但解析仍按 path。

## Why the bucket was there

`Skill::id` 把 bucket 写进名字，是因为一个名字可能在两个 root 里各有一份，而旧 `at()` 用
bucket 决定去哪个 root 做一次 `stat`（不遍历）。SPEC 的 `Skill := path` 是这个拼写的依据。

## The rule this settles on

一个 skill 的**名字**就是它的 id；**bucket 是那一行的一个字段，不是寻址的一部分**。

- `Skill::id()` 返回 `name`。
- `Roots::at(id)` 仍然按 path 读：一段是名字（按 `Bucket::ALL` 顺序试各 root，最具体的那份
  赢）；两段以上是某个 root 下的子目录（`harness/a/b`）—— 未来子目录 skill 的地址就是这一支。
- `ls_skill --path` 匹配 skill 名**或** bucket 名：`harness` 仍是 home 的全部，`har` 仍
  匹配不到，`self-control` 是它自己。
- `run_skill(id)` 走 `roots.at()`：一个名���就是名字，一个 bucket 下的子目录路径也仍然解析。
- `session_roots(cwd, machine)` 是新的唯一构造点：`installed_for`（walk）与 `at`（按名字/路径
  解析）来自同一组 root，所以"列出来的名字"就是"跑得起来的名字"。

## 同名时：`at` 取最具体的那份，和 `@skill(...)` 一致

一个名字在两个 root 都在时，`ls_skill` 列两行（各自带 `bucket`），`run_skill` 跑最具体的
那一行。不额外报错：SPEC 已经规定"更具体的层赢"，而 `@skill(...)` 一直是这么读的；再加一条
拒绝就等于两个规则打架。要跑另一份，先 `ls_skill <bucket>` 看它来自哪层。

## Deliberate consequence

`ls_skill` 的行从此以 name 为 `id`，`bucket` 字段仍在。旧的 `harness/x` 拼接不再是 id；
调用方（含 `scripts/*_selftest.sh`）改用 `x`。文档同步：`docs/notes/server-commands.md`
的 `ls_skill`/`run_skill`、`docs/notes/server-content-commands.md`、`docs/notes/skill-lib.md`
的 `at` 一节。