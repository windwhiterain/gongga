# 内容按 protocol 解析，内容仓与本机仓同构

- 日期：2026-11-03
- 结论：`crates/server/ship.toml` 不再写 commit；一个 home 把内容仓当 checkout（官方
  `main` + 本机 `local`，rebase），装「说它读的那个 protocol 的最新 commit」。
- 为什么：pin 意味着改一句 prose 要在本仓库留一个提交并重新编译，而两个名字指同一个
  commit 正是会漂移的那一类（`AGENTS.md` 里记过一次）。内容仓的提交与本 build 无关，
  所以 `.gitmodules` 的 `ignore = all` 才成立。
- 复用：`fork` 模块把「fetch → ff 官方 → rebase 本机」写成一处，社区 roster 与 ship 共用。
  分支名从 `fork.rs` 读（`scripts/ship.sh` 解析常量），两个调用方不会各叫各的。
- merge base 变了：以前是「pin 那个 commit 的 blob」，现在 rebase 会重写 commit，所以
  base 是「上次装下来的字节」（`<home>/.ship-base/<root>`）。老 home 只有 pin 没有 base，
  `seed_base` 从 pin 的树补一次。
- 升级不需要网络：老的 `<home>/ship.git` 被 `adopt` 成 `<home>/ship` 这个 checkout，
  再按 protocol 走。
- 发布 = `scripts/ship.sh -m ...`：提交 `local`、rebase 到 `main`、push。`--check` 只回答
  「这个 build 读得懂它要推的内容吗」，退出码 0/1/2/3 语义不变；`--push-main` 已删（它
  命名的是不存在的动作），`--local` 给 commit 用。
- 验证：`scripts/ship_selftest.sh`（PASS 84s）。临时仓库 + 临时 home + 真服务端：protocol
  回落、个人 commit 装上且活过上游变更、无友好 commit 时拒绝并报两个 protocol、老 bare
  缓存被 adopt、发布路径的上游 rebase。另：`~/.gongga` 真换手后
  `the old content cache is now a checkout` + `shipped content content=19076b18da
  updated=6 merged=1`。
- 未做：`pool.py` 仍接受不点档的 `pick`（skill 正文已要求点档，工具不拦）。
