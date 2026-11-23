# 容量驳回驱动的 unmasked_ctx_limit（WIP）

分支 `unmasked-ctx-limit`，worktree `.worktrees/unmasked-ctx-limit`。

## 已实现

- SPEC「Memory Mask」新增三条：会话级 `unmasked_ctx_limit` 初值 = provider 的
  `unmasked_context`（× `ctx_limit`）、每次显式容量驳回减半且不低于普通尾部、连续成功
  升一档且封顶；每次下调提交 cause `refusal` 并推进 `view_generation`。
- `crates/server/src/llm.rs`：`capacity_refusal(message)` —— 只认 503/507 与明说设备内存
  的措辞；超时、断连、坏答案、4xx 参数错都不算。
- `crates/server/src/memory.rs`：`memory_state` 新增 `unmasked_ctx_limit` /
  `unmasked_ctx_streak`（幂等 ALTER 迁移）；`Store::unmasked_ctx_limit`、
  `record_capacity_refusal`、`record_unmasked_success`；FIFO 头驱逐抽成 `mask_fifo_head`
  供限制与驳回共用。
- `crates/server/src/agent.rs`：`memory_token_limit` 取会话级值并夹到 ceiling；
  驳回时下调→记录事件→重新走一步（不是重发同一请求）；成功计数每 3 次升一档。

## 判据

- `mbx build -p gongga-server` 通过。
- `mbx test -p gongga-server --lib memory::tests` 4 passed；
  `--lib agent::` 12 passed。
- 门禁：`mbx fmt --check` 干净、`test-layout-check` 0 violations、
  `scripts/skill_contract.sh` PASS、`comments-check` 与基线同为 10 条（无新增）。
  `ship.sh --check` 在 worktree 里没有 `ship` checkout，未判。

## 未做

- 真进程 e2e：需要一个 gongga 服务端跑新代码 + 一个会话在 8312 上撞到 503，观察
  `memory_limit` 事件出现 `cause=refusal`、`unmasked_ctx_limit` 减半、下一次请求变小。
