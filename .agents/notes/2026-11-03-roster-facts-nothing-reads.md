# roster 里写着、却没人读的事实

缓存保留时间那一件事（`cache_ttl_secs`，2026-11-02）问出来的是个更大的问题：
roster 是**一台机器关于厂商的事实表**，而服务端只解析其中一小撮字段，其余全是
"给人看的散文"或者"给别人的脚本看的字段"。散文本身不是问题；问题是散文里写的
**是个会改变行为的事实**，而没有消费者。

下面按"被丢掉会怎样"排序。数字都是 2026-11-03 这份 roster 数出来的。

## 一、`max_output`（26/34 个模型写了，没人读）

`[model] max_output_tokens` 只在机器 config 里，ship 默认注释掉 → `None` →
请求里不带 `max_output_tokens` → 由厂商的默认值决定答案长度。

roster 已经知道每个模型能出多少（deepseek-flash 384000、kimi 131072、doubao 262144、
loca-qwen3 不写），而 harness 一个都不发：模型写一个长文件时被厂商默认值截断，
截断本身不报错。`max_input`（10 个模型）同理：压缩提醒按 `context`（总量）算，
不看"输入侧只有这么多"。

## 二、`retired_ids` / `alias`（4 个模型有 retired，2 个有 alias，没人读）

`skills/model_pool/roster.py` 用 `retired_ids` 把退役 id 排除出抽签；`providers.toml`
自己的 `retired_note` 写的是"另一个 id 承接这个模型"。但**服务端不查这张表**：
`set_session --model` 存下来的是字符串，厂商一改名（`doubao-seed-1.8` → `-2-1-pro-260915`、
`deepseek-chat` → `deepseek-flash`），那个会话的下一回合就以"provider has no model"失败，
而这行映射就在文件里。`alias` 是同一件事的另一半：点号写法 `doubao-seed-2.1-pro`
是展示名，roster 说了它等于哪个 id，服务端只认 id。

## 三、价格的第二层：`cache_write*` / `cache_read_explicit` / `cache_storage*` / `band`

`Price` 只读 `input`、`cache_read`（`output` 读了不用），于是：

- 写入价（kimi k3 系 5m 13 / 1h 26，qwen 显式 15）与命中价分档（`cache_read_explicit`）
  不进规划。**今天不算错**：这个 harness 从不发 `cache_control`，所以显式缓存与写入费
  根本不发生 —— 但这句话没有写在任何地方，是"碰巧对"。
- 存储费（doubao 0.017 /M·小时、zhipu 限时免费 0）不进规划：热着的链被当成免费持有。
  量级小（100k 前缀放一天 ≈ ¥0.04），可它是*按小时*的，正是 TTL 的另一面。
- `price.band` 只有 `providers/price.py` 读。四个分时/分档模型的 `input/cache_read`
  比值在各档里一致（deepseek 50/30、minimax 5、mimo 120），所以 plan 用扁平值没有算错
  —— 但 deepseek 的 `peak` 档**不写 `cache_read`**，谁只读 band 谁拿到空。

## 四、`cached_tokens`：唯一的实测被丢掉

每个 step 的 `usage.cached` 都进了 `events` 行和会话卡片（TUI 显示 `cached n%`），
而 plan 用的是**声明**：`cache_ttl_secs`（厂商/估计）和 `cache_invalid_rate`（全机一个
超参数）。也就是说，"这次重放命中了 98%" 这件事机器知道，却没有任何路径拿它去修正
`invalid` 或验证 roster 的 `cache_ttl_estimate`。

这是这件事的正经下一步：`Rates.invalid` 按模型做后验（命中/期望命中），
再把测出来的数回头对一次 roster 的估计 —— 否则 roster 里那三段散文永远只是散文。

## 五、只是给人读的（不需要消费者，列出来免得下次再查一遍）

provider 级：`kind` `homepage` `docs` `catalog_verified` `retrieved` `note`
`plan_kind` `regions` `catalog` `plan` `console` `key_prefix` `tokenhub_plan` `plan_note`
`base_url`（自建那一个）`served_by`；`cache_ttl` `cache_ttl_note` `cache_ttl_source`
`cache_ttl_estimate*`（数字形态是 `cache_ttl_secs`）。

model 级：`params`（model_pool 读）`note` `reasoning_note` `doc_conflict` `image_limits`
`output_modalities` `compat` `rate_limits` `thinking_budget_max` `concurrency_limit`。

`quota_query`：`url` `jsonpath` `auth` `body` 等描述性字段 —— 服务端只跑 `script`，
文档里已明说。

## 顺带一条：ship 默认 config 里有一句现在是错的

`[agent] cache_invalid_rate` 的注释仍写着"raise it for a provider whose cache is
short-lived"：这个 knob 是**全机一个**概率，按 provider 调不了；短命的缓存现在由
roster 的 `cache_ttl_secs` 精确管住，两者叠在一起会重复计价。下次动 ship 时把这句改掉。
