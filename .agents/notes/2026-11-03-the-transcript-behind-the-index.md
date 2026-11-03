# 记账里漏了最大的一笔：index 后面那 13 万 token

用户问「每次请求都重温了它对应的 cache 节点吗」，我说「是，对 plan 选中的链」，然后他
指出真正的问题：**transcript 在 prefix cache 的账本里根本没有节点。**

## 事实（改前）

- `plan` 只被两处调用：`index_of` 的 `"prompt index"`、`index_skill` 的 `"skill table"`。
  `Wanted` 只由引用根和技能表构造，消息不进 layout。
- index 被拼进 `instructions`（`llm.rs` 的 `build_head`），而 `instructions` 在请求前缀里
  **位于所有 `input` item 之前**。
- 于是 provider 缓存的前缀是：`tools` + `instructions`(prose + index) + `input`(卡片 + transcript)。
- `plan` 的成本模型只数 index 那几个条目的 token（几千），transcript（十几万）不计入。
  它可以在「重排 index 省 1000 token」与「transcript 整段重算 130000 token」之间选前者。

steady state 之所以看着没事，是因为 `prompt_in_force` 判等不写新行、transcript 又是逐字节
重放，所以字节没变、命中自然高。但那是**别的机制**在保它，不是 plan 知道它值钱。

## 改法

1. `Rates::following_tokens`：这个请求在 index 之后要带的 token 数。
2. `Layout::last`：上一次真正发出去的 emission 的 item id 序列（不是节点，因为问的是
   "这次是否重复了 provider 手里的字节"，节点只答"我哪些祖先还温着"）。
3. `plan` 给每个候选 emission 加一笔：与 `last` 相同按命中价，不同按 miss 价；
   baseline（调用方顺序、什么都不温）同样计价。
4. `index_of` 用 provider 自己数的 last input tokens 减去 index 得到这个数；
   `index_skill` 的 `Rates` 是 0（工具结果在 transcript 中间，后面没有请求负载）。

## 顺带确认的两件事（用户追问过，代码里写着）

- `plan` 不改 transcript：它只决定 index 那一个字符串的次序，`Prompt::rewritten_by`
  判等就 `return kept`，只有真变了才 `append_prompt`。被冻结的是"发出去的话"。
- 于是"每次请求都重新 plan"和"落盘不许改"不冲突：重算的是排序器，落盘的是渲染结果。

## 留在桌上的

- 布局按 session 存、不按 model 存：切模型后节点和 `last` 跨模型共用。失效场景是
  A 上温着的次序被 B 的重排改写后，切回 A 时按 B 的次序发。改法：`Layout` 里记 model，
  不一致就当空布局。
- 乐观 `touched`：`plan` 在发送前就记账，失败/取消的请求也算"刚发过"。
- `usage.cached_tokens` 仍然只进日志与卡片，不回填 `Rates::invalid`。
