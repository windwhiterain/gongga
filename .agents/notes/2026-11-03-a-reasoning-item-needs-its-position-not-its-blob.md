# A reasoning item needs its position, not its blob

日期：2026-11-03

## 问题

`2026-10-30-tool-call-replay-is-reordered.md` 的变体表里有一行：

| 变体 | 结果 |
| --- | --- |
| 只有 text 没有 `encrypted_content` | 400（所以别放弃 blob） |

这行被当作"半截思��不能存成 reasoning 行"的依据。它没有可复现的记录：谁跑的、
什么请求、什么时间都不知道。所以自己量一次。

## 方法

直接打 `POST https://api.deepseek.com/v1/responses`，`deepseek-flash`，
`reasoning.effort=high`，带一个 `tools` 声明（不带 tools 时 thinking 模式的回传规则
根本不生效，200 说明不了任何事）。对话里有一个 `function_call` 和它的
`function_call_output`，只改其中一个 item 的形状。

每条 200/400 都是 provider 自己的答复，不是推断。

## 位置：真会 400

| 变体 | 结果 |
| --- | --- |
| reasoning 在 call **之前** | **200** |
| reasoning 夹在 call 和它的 output **之间** | **400** `No tool output found for tool call call_p1` |
| reasoning 在 output **之后** | **200** |
| assistant text 夹在 call 和它的 output 之间 | **400** 同上 |

所以"call 必须紧跟它自己的 output"是真的、可复现的，而且**任何** item 夹在中间都会
触发——不只是 reasoning。

## 带不带 blob：复现不出来

同一份对话，reasoning item 只改它携带文本的方式：

| 变体 | 结果 |
| --- | --- |
| 文本在 `content: [{type: reasoning_text}]`（provider 实际返回的形状） | **200** |
| 文本在 `summary: [{type: summary_text}]`（harness 存的形状） | **200** |
| 完全不携带文本 | **200** |
| 不带 `reasoning.effort`，只有文本 | **200** |

没有一种 shape 因为"缺 `encrypted_content`"被拒。进一步：连一个**假的** blob
（`encrypted_content: "not-a-real-token"`）也被 200 接受。所以在这个配置下 provider
根本没有校验这个字段的内容，更不会因为缺它而拒绝。

一个观察：这个 provider 的 `encrypted_content` 是个 38 字符的 token
（`690dd375-ef0a-4a31-89b6-ca0415458b78-0`），不是密文；而且只有请求带
`include: ["reasoning.encrypted_content"]` 时才出现，两次同样的请求一次给一次不给。
harness 库里 1320 条 reasoning 行全部带着它。

## 结论

- **位置规则**：成立，可作为设计依据。`function_call` 必须紧跟自己的
  `function_call_output`，中间不能有任何 item。
- **blob 规则**：不成立（至少今天、这个模型、这个账号上）。"半截思考没有 blob
  就会被拒"没有证据支持。
- 因此"半截思考只能存成 `notice`"是**过度保守**：它安全（永不进请求），但也白丢了
  模型本该读到的上下文。仓库当前就是这么做的，见 `docs/notes/server-store.md` 的
  drafts 一节；若要改成存 `reasoning` 行，位置必须落在它自己那一步的 call **之前**或
  最后一个 output **之后**。
- 这个探针**没有**证明"reasoning 必须原样回传"这条官方规则不生效——它只证明了
  本 harness 现在的请求形状不会因为缺 blob 而被拒。

## 未复现的部分

`reasoning_text in the thinking mode must be passed back` 那个原始 400（2026-10-30）
今天没再出现。可能已经修复，可能是被位置规则掩盖（那条历史里 reasoning 正好夹在
两个 call 之间）。没有旧请求体，无法区分。
