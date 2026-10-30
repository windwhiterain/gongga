# 带上 tool call 的第二轮被 DeepSeek 拒：reasoning 必须原样回传

> **最终结论（以官方文档为准）**：DeepSeek《Thinking Mode》指南写明——请求一旦带
> `tools`，每个 assistant 轮的 reasoning 必须在**所有后续请求**里原样回传，包括那一轮
> 没有发生工具调用的；截断或摘要也会被拒。所以"丢掉 reasoning"是错的，正是它导致
> `reasoning_text ... must be passed back`。修法改为**把 provider 返回的 reasoning item
> 整条存下来、整条发回去**（`HistoryItem::Reasoning.item`），旧行没有这个字段时按
> blob 重建。顺序规则（call 紧跟自己的 output、reasoning 在 calls 之前）保留，但不再
> 丢弃任何 reasoning。
>
> 证据：https://api-docs.deepseek.com/guides/thinking_mode（Tool Calls 一节）
> 以及 https://github.com/AAswordman/Operit/issues/1283 里同样的 Responses 路径 400。


日期：2026-10-30

## 症状

session 里第一次请求 200，第二次（把 tool call 结果回传）400：

```
The `reasoning_text` in the thinking mode must be passed back to the API.
```

重跑同一会话后换成另一个说法：

```
No tool output found for tool call call_00_jXpZ...
```

## 根因

不是 reasoning 丢了，是**回放顺序**。DeepSeek 的 Responses 兼容层按结构校验：

1. 一个 `function_call` 必须**紧跟着**它的输出。两个 call 的输出如果按反序落库
   （`call_a, call_b, out_b, out_a`），就报 `No tool output found`——它按位置配对，不按 `call_id`。
2. 一轮里的 reasoning 必须在该轮的 call **之前**。模型在两个 call 之间又思考了一次时，
   落库顺序是 `reasoning, call_a, reasoning, call_b`，原样回放就报 `reasoning_text must be
   passed back`——文字在，但不在 provider 找的地方。

## 证据

对真实 body 做变体测试（同一份会话、同一个 blob）：

| 变体 | 结果 |
| --- | --- |
| 原样回放 | 400 |
| reasoning 提到 call 之前 | 200 |
| 去掉中间那条 reasoning | 200 |
| 改成 `call→自己的 output` 相邻 | 200 |
| 只有 text 没有 `encrypted_content` | 400（所以别放弃 blob） |

## 修法

`llm.rs` 在 `build_body` 里加一次 `normalise`（只改请求，不改抄本）：

- `drop_orphans`：输出还没落库的 call 直接不发（比整个请求被拒强；模型会重新决定）；
  没有对应 call 的输出同样去掉。
- `reassemble`：每个 step 的 reasoning 提到该块最前，然后按 call 顺序输出
  `call → 它自己的 output`。

抄本里的行序保持原样，所以 `get_messages`、report 看到的还是真实发生过的顺序。

## 第三版（已作废）

曾经加过"块里只剩 reasoning 就整块不发"的守卫。它确实能让那个形状不再 400，但方向错了：
官方要求 reasoning 必须回传，丢掉它是在制造下一种 400。已被"原样回传"取代。

## 验证

拿真实坏历史（reasoning 夹在两个 call 之间）在副本 home 上重跑：修复前 400，
修复后连续 4 次 200，工具正常执行、这一轮继续往下跑。

## 试过又撤掉的

- 回放时补 `content: [{type: reasoning_text}]`：不解决问题，仍 400。DeepSeek 要的是位置。
- 丢掉所有 reasoning 只发 call/output：能用，但白丢 blob（缓存与连续性），不取。

## 留下的

- 排障期间 `llm.rs` 里临时加过 `GONGGA_DUMP_REQUEST`（把请求体落盘），验完已删。
- `session recheck` 里多了两条我发的 `继续，只回一句…`（seq 10、22），是排障时触发的；
  要干净就 `del_session recheck`。
