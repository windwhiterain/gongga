# handoff 笔记要按首 prompt 落行

User 要求：handoff 清空 session 之后笔记要走 `new_session` + `send_message`
的首条 prompt 路径，能触发 prefix cache plan，构造出的 context 与全新会话一致。

## 洞在哪

2026-11-09 把 harness 注入从 `text` 改成了独立的 `inject` kind（为了 UI 上
青色块），但 `index_of` 仍只读 `store.user_texts`（`kind='text'`）。handoff
笔记经 tool-origin 的 `send_message` 排队，在 `prompt_in_force` 之前由
`deliver_pending` 落成 `inject` 行：队列里已经取走，`user_texts` 又看不到它。
于是笔记里的 `@file/@skill` 保持字面，却没有进 index；prefix cache plan
根本没被笔记触发。这与 2026-11-02-heir-lesson-is-a-message 的承诺相反。

## 改法

- `commands::send_message`：tool-origin 的消息若目标 session 没有 prompt row
  （handoff 清空后就是这样），判定为 `first_prompt`，传给 `deliver`。
- `commands::deliver`：把 `first_prompt` 写进 inbox 任务的 payload；空闲
  session 本来就走 `begin_turn` 写 `text` 行，不需要标记。
- `store::deliver_pending`：有标记的任务落成 `Kind::Text`（`injected: false`），
  其余仍是 `Kind::Inject`。inbox 任务种类不变，`queued_texts` 也仍能读到它。
- origin 仍是 tool，所以 `keepalive_handoff_at` 不被清除，心跳静默不变。
- 在飞的 tool_call / tool_result 仍留在 transcript：provider 要看到那次调用的
  结果；head（system/card/index/parameters/tools）已与全新会话同构。

## 验证

待补：mbx build 已过；fmt / selftest / 门禁见本轮工具输出。
