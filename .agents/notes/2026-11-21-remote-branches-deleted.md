# Remote branches: 远古未合并的提交，先查后删

用户的问题：远端有一批很久没合并的分支，「看看如何合并」。我列出三条方案（整体
rebase 合、只挑需要的提交、全部丢弃），用户回：**「有用的 cherry pick，无用的删」**。
这不是合并请求，是删除 + 挑选的授权。

## 删了什么

`git push origin --delete` 删掉 9 个分支，`git fetch --prune origin` 清掉 tracking
ref。删前确认没有本地分支以它们为 upstream（`git for-each-ref
--format='%(refname:short) -> %(upstream:short)' refs/heads/` → 无 `origin/*`）。

| 分支 | tip | 独有提交 | 判定 |
|---|---|---|---|
| bench-update | 394be9ab | 0 | 纯落后 |
| exec-line | 5eea2284 | 0 | 纯落后 |
| upstream | 9c1af563 | 0 | 纯落后 |
| feature/client-split | fc13fc89 | 0 | 纯落后 |
| keep-test-drop | f143356d | 1（空提交，`git diff-tree` 文件数 0） | 无内容 |
| backup-before-transfer | c5424e0c | 8 | main 的 `no_text` 谓词更完整（`store.rs:2886` 判「有行但无 text 行」，它判「表非空」），`live_compact::Commit` 也在 |
| feature/live-compact-effect-3 | e44f8b0d | 5 | store 三项 main 已有（`agent.rs:580`「head written once」）；`Look::Blind/AlreadyDirty/Unversioned` 是改写不是 cherry，`git apply --check` 已不适用 |
| exec-merge | 79e63b46 | 1 | 只含 `FAKE_LLM_HOST`（已挑走）与 `Cargo.lock` 的 libc（main 已有） |
| backup/main-2026-11-21 | 66351255 | 7 | 只含 gate 竞态修复（已挑走）；paste/byte-stream/DEBUG_KEYS 皆在 main（`vt_input.rs:71`、`lib.rs:834`） |

四个 tip 都先钉成本地 tag，删远端引用不丢提交：

```
backup/before-transfer        c5424e0c
backup/exec-merge             79e63b46
backup/main-2026-11-21        66351255
backup/live-compact-effect-3  e44f8b0d
```

复原：`git push origin backup/<名>:refs/heads/<原分支名>`。

## 挑走了两处（`190472d2`）

1. **gate 在 turning ending 之后才释放**。原 patch 已不适用（main 的 `run_turn`
   签名与 `turn_inner` 参数都变了：`turn_inner` 现在收 `_gate`），所以按今天的形状
   重写：`run_turn` 里 `let _gate = gate;`，`turn_inner` 不再收 gate，guard 在
   `finish_turn` 与 status publish 之后 drop。main 的 `agent.rs:124` 注释当时还写着
   `gate is free once turn_inner returns`，即为证据：那段空窗真实存在，
   `nudge_inboxes` 两秒 tick 的 `claim_recovery` → `try_claim()` 能抢到。
   判据记录在 `docs/notes/server-agent.md` 的 `the_next_turn_waits_for_the_ending`。
2. **`FAKE_LLM_HOST`**：`scripts/fake_llm.py` 绑 `127.0.0.1` 改为可覆盖，容器里的
   harness 经 host gateway 访问。注释改成英文（本仓库代码/注释写英文的规矩），
   并记进 `docs/notes/server-llm.md`。

## 未做的

`Look` 三态（`Blind` → `Changed/AlreadyDirty/Unversioned`）没有挑。它不是
cherry-pick 能做的形状：main 的 `explore.rs` 已长出 `hash_paths`，原 hunk
`git apply --check` 失败。要不要做是一次独立设计，不是这次清理的一部分。

## 一处既有红

`scripts/drain_selftest.sh` 的最后一条断言仍在找 `server_stop` 里的
`"killed":0` / `"released":`，而 `e393449f`（2026-11-14）已把这两个字段从 payload
去掉（`git show 3859bfcd:crates/server/src/commands.rs | grep -c '"killed"\|"released"'`
→ **0**）。所以 `FAIL a client cancel lets jobs go and kills none` 在我改动之前就是红的，
不是这次的回归。断言与实现谁该改，需要用户判断（改断言 = 承认 payload 就是
`by/sessions/pending/channels/kept_channels` 这五项）。