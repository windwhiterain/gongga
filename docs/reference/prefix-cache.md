# Provider prefix caches: modes, lifetimes, billing

What the providers we talk to actually sell, so a plan can be argued about instead of
guessed at. Not what this server does: what it must fit.

Sources, fetched 2026-11-03:

- Kimi — <https://platform.kimi.com/docs/guide/context-caching>
- MiniMax, automatic (the OpenAI faces this server speaks) —
  <https://platform.minimax.io/docs/api-reference/text-prompt-caching>
- MiniMax, explicit (the `/anthropic` face) —
  <https://platform.minimax.io/docs/api-reference/anthropic-api-compatible-cache> — **this URL
  now redirects to the automatic page**: checked 2026-11-05, the explicit cache's
  `cache_control` rules (breakpoint, 20 blocks, 4 marks, 5 minutes) are no longer on the
  provider's site. What the page says today is quoted under "what MiniMax documents
  now", below.
- Volcengine Ark — <https://docs.volcengine.com/docs/ark/context-cache>
- Alibaba Model Studio — <https://help.aliyun.com/zh/model-studio/context-cache>

The numbers below are those pages' own plus what `~/.agents/gongga-agents/providers.toml`
records for our thirteen providers; the roster is where a price is *read* from, and this
file is where the shape is explained.

## Three questions, and they are separate

"Does it cache" hides three decisions that cost different money:

1. **Who decides what is cached.** Implicit (the provider hashes the request prefix),
   an explicit marker (you name a breakpoint with `cache_control`), or an explicit lease
   (you create a cache with an id and an expiry, and reference it afterwards).
2. **How long it lives, and whether a hit renews it.** Renewing, deadline, or evicted.
3. **What it costs.** Input, hit, write and storage are four separate rates; batch,
   peak/off-peak and service tiers are discounts that multiply them.

A plan that mixes them up is wrong in a way that never shows up as an error: the request
succeeds, the answer is right, and the bill is a multiple of what it should be.

## The three modes

| mode | how a request asks for it | who creates the cache | hit determinism |
| --- | --- | --- | --- |
| **implicit** | nothing — it cannot be turned off | the provider, from the request prefix | not guaranteed: capacity eviction and routing decide |
| **explicit marker** | `cache_control: {"type":"ephemeral"}` on a content block (Anthropic / Chat Completions shapes) | you choose the breakpoint, the provider caches up to it | deterministic while the block lives |
| **explicit lease** | Volcengine: `store: true` + `caching: {"type":"enabled"}` + `expire_at`, then `previous_response_id` | you create it, the provider returns an id | highest: the id is the cache |

Field names, as each vendor spells them:

| vendor | selecting a tier | route affinity (free) |
| --- | --- | --- |
| Kimi | `prompt_cache_options: {"mode":"implicit","ttl":"5m"\|"1h"}` on Chat Completions **and** Responses; `cache_control: {"type":"ephemeral","ttl":...}` at the **top level** of Anthropic Messages | — |
| Volcengine Ark | `caching: {"type":"enabled","prefix":true}`, `expire_at` = now + 3600 … 604800 | `prompt_cache_key` (≤15 req/min per value) |
| Alibaba | `cache_control` on a content block, at most 4 marks per request, look-back 20 blocks | — |
| MiniMax | automatic on `/v1/chat/completions` **and** `/v1/responses`; `cache_control` only on the Anthropic surface | — |
| Tencent TokenHub | — | `prompt_cache_key` in the body and an `X-Session-ID` header, to pin the inference instance |
| DeepSeek, StepFun, LongCat, Dots, Zhipu, local | — (implicit only) | — |

### what MiniMax documents now

`platform.minimax.cn/docs/api-reference/text-prompt-caching` (raw markdown at `.md`),
fetched 2026-11-05, says caching is **automatic** -- its own Anthropic SDK example sends
no `cache_control` -- and states the boundary in one sentence:

> 缓存采用前缀匹配的方式，以「工具定义-系统提示词-历史对话内容」为顺序构建，任意模块内容的变更，都可能会影响缓存的效果；

with, beside it: 512 or more input tokens; and the best practice *静态内容在前、动态用户信息在最后*.
The page names the **order** in which the prefix is assembled and says a change in any
module *may* affect the cache. It does not say where a cut falls inside a module, and it
documents no `cache_control` anywhere -- its own Anthropic SDK example sends none. The
page the older reading of this file came from (`anthropic-api-compatible-cache`, with its
breakpoint / 20 blocks / 4 marks / 5 minutes) redirects here today, so that reading is
withdrawn rather than quoted.

### the boundary this provider actually has

Measured 2026-11-05 against the real endpoint, with the messages long enough for the
answer to be readable: the read is quantized to 128 tokens, so a message of a few dozen
tokens cannot locate a boundary inside itself. Three messages of ~1,200 tokens each, on
top of the machine's 15 tool schemas, the stored prose and a stored 51,310-character
index:

| send | total | read | read/128 |
| --- | --- | --- | --- |
| `[u1]`, never sent | 24,857 | 0 | |
| `[u1, a1]` | 28,467 | 24,704 | 193 = floor(24,857/128) |
| `[u1, a1, u2]` | 32,076 | 28,288 | 221 = floor(28,467/128) |
| the same again | 32,076 | 31,872 | 249 = floor(32,076/128) |
| `u2` extended by one word | 32,077 | 31,872 | the stored request's own length |
| **`u2`'s first character changed** | 32,076 | **28,288** | the length of `[u1, a1]` |
| **`a1`'s first character changed** | 32,076 | **24,704** | the length of `[u1]` |
| **`u1`'s first character changed** | 32,016 | 192 | nothing: no stored request ends before it |

Those rows put the cut at the **start of the message** that changed, and they do not
prove a message is one unit -- a divergence at a message's first character lands in the
same place under both readings. Changing a word *inside* a 900-word message says which it
is:

| send (the same stored base as above) | total | read |
| --- | --- | --- |
| `u2` changed 10% into it | 32,078 | 28,672 |
| `u2` changed 50% into it | 32,078 | 30,080 |
| `u2` changed 95% into it | 32,078 | 31,744 |
| `a1` changed 50% into it | 32,078 | 26,496 |

The served length follows the change point through the message, so **the boundary is a
token prefix, quantized to 128 tokens**; a message boundary is only the special case where
the change begins there. An appended message, and a fork taken anywhere after a stored
request's last message, are served to that point.

## the readings that do not reproduce

Two structurally identical pairs came back differently in one session, which is why no
single reading here can be used as a rule:

| pair | read |
| --- | --- |
| first message differs by one character (`alpha` -> `a1pha`), rest identical | 21,120 |
| first message differs by its name (`alpha` -> `beta`), rest identical, same lengths | 189 |
| the last character of a 21k prompt changed, messages merged | 192 |
| the same, messages alternating (the `u2`/`a1` rows above) | 28,288 / 30,080 |

So the *mechanism* is a token prefix, and the *service* of that prefix is not reliable on
this account: identical structures answer anywhere between 0 and the whole prefix. What is
reliable is only the case with no divergence at all -- repeating a request, or extending
one -- which is what a conversation does on its own.

Two blocks above the messages behave differently, both measured: the **system text is one
unit** (appending a single space to it answers 189 -- the tools do not survive a system
change), and the **tool definitions are one unit** (one description changed answers
16,128 of 21,228, which is the system and nothing after it). Their order on the wire is
**system, then tools, then messages**, which is the reverse of the order the provider's
page states; the page describes how it assembles a prefix, not what it serialises.

That also corrects an earlier reading of this file: a session's `cached_tokens` column is
its *newest* measurement, and a turn that calls a tool sends two requests, so "five fresh
sessions hit 15,616 of 15,640" was measuring each session's second request inside its own
turn. The cross-session case, isolated: four siblings sharing the whole head answered
128, 128, 0, 128.

What follows for a plan: the transcript's own shape already earns the 98-99% (every step
appends a message to the last request), and the only cross-session reuse reachable is
siblings whose messages are identical from the first one on -- which the start card's
session name and minute forbid. No ordering of the index can buy that, because the system
text is a single unit and it comes first.

MiniMax's automatic cache states three things worth writing down, checked 2026-11-05:
caching applies to requests of **512 or more input tokens**; the prefix is matched in the
order **tool list → system prompts → user messages**; and no lifetime is stated. Its
explicit cache is a different face and a different rule set: a `cache_control` breakpoint
marks the end of the cacheable prefix, the cache is cumulative and checked forward from
the breakpoint, at most **20 blocks** back, at most 4 breakpoints, and it lives **5
minutes, renewed on every hit**. `prompt_cache_key` is not documented on either face, so
a reading of that surface cannot be explained by a key.

Two things follow from the table and are easy to miss:

- A tier can be selectable on **one surface but not the one this server speaks**. Kimi's
  is on both -- the only one that is, today. Volcengine's explicit lease needs
  `previous_response_id` chaining and an empty `instructions`, the opposite of the shape
  this server sends (a full transcript plus `instructions` on every request). Alibaba's
  marker is documented for Chat Completions, DashScope and Anthropic Messages, with a
  separate **Session** cache configuration for its Responses surface; MiniMax's is the
  Anthropic surface.
- Route affinity is not caching: it creates nothing, costs nothing, and only raises the
  probability that a request lands where its prefix already is. It is worth taking
  wherever it is offered, because it is the only free knob in the table.

## Three lifetime semantics

| semantics | who | what it means for a plan |
| --- | --- | --- |
| **renewing** | Kimi 5m/1h, MiniMax **explicit**, Alibaba explicit, most implicit caches | every hit resets the clock, so a gap shorter than the TTL keeps the prefix warm indefinitely; `last sent + ttl` is exactly when it dies |
| **deadline** | Volcengine explicit `expire_at` (official: "不会随着缓存 / 存储的使用而重置缓存生命周期") | the clock starts at the **write**; using it does not move it, so the death time is `written + expire_at`, not `last sent + ttl` |
| **evicted** | DeepSeek, StepFun, LongCat, Tencent, Dots, Zhipu, Alibaba implicit, Volcengine implicit, **MiniMax automatic** | there is no TTL to obey; the provider clears what it has not seen for a while. Only a probability is honest |

The distinction is the whole reason `prefix_cache::expire` cannot simply take "a TTL":
its `touched` is refreshed on every send, which is right for the first row and wrong for
the second. Today every tier in force for this machine is renewing or evicted, so the
algorithm stands; a leased deadline tier would need the write time recorded beside the
chain.

## The bill

```
cost = (input_tokens − cached_tokens) × input
     +  cached_tokens               × hit
     +  newly written tokens        × write      ← explicit tiers only
     +  stored tokens               × storage × natural hours   ← leased tiers only
     +  output_tokens               × output
```

then multiplied by whatever discount applies to the request (batch, `service_tier`,
peak/off-peak band).

- **hit** is the rate that matters. In our roster `input / cache_read` runs from **free**
  (dots3, `cache_read = 0.0`) through 3.5× (zhipu), 4–6× (zhipu, doubao, step-3.5/3.7,
  MiniMax-M3, hy3), 8× (qwen3.8), 10× (kimi k3/k3.1-pro, step-5, MiniMax-M3.1),
  20× (kimi highspeed, hy4), 30× (deepseek-v4-pro), **50×** (deepseek-flash, LongCat) to
  **120×** (mimo-v2.6-pro).
- **write** has three shapes in practice: **1×** input (Kimi 5m — official: "写入成本原先就
  包含在输入价格中，拆分后现有请求的整体费用不变"), **1.25×** (Alibaba explicit:
  `cache_write` 15 vs `input` 12), **2×** (Kimi 1h: `cache_write_1h` 26 vs `input` 13).
  Volcengine's explicit tier has no write premium; new tokens are ordinary input.
- **write is charged on newly written tokens**, not on the whole prefix: a request that
  hits an existing prefix and extends it pays write only for the extension. Alibaba says
  this literally ("仅对新增部分计费"); Kimi's phrasing is "前缀首次写入缓存时一次性收取",
  which is the same thing read together with its "总价不变" note. Treat the equivalence as
  inferred for Kimi, measured for neither.
- **storage** is per million tokens per **natural hour**, billed at the top of the hour,
  a partial hour counting as a full one (Volcengine). In our roster: **0.017** (doubao),
  **0.00** for now (zhipu "限时免费"), and zero for every implicit cache — Volcengine states
  the contrast explicitly: "与显式缓存相比，隐式缓存存储不计费".
- **discounts are orthogonal and badly under-used here**: batch is 60% of standard for
  Kimi and half price for mimo's `batch` band; `service_tier=flex` is about half for
  Volcengine; peak/off-peak exists only for DeepSeek and part of Tencent, and DeepSeek's
  `peak` band carries **no** `cache_read` at all — a reader that takes the band without
  falling back to the flat price gets nothing.

## The account that decides a tier

For a prefix of `P` tokens, one write and `n` later hits:

- tier `a` costs `w_a·P + n·h_a·P`; tier `b` costs `w_b·P + n·h_b·P`, so `b` wins when
  `n·(h_a − h_b) > w_b − w_a`.

Kimi's two tiers have the same hit rate (¥2) and differ only in write (¥20 vs ¥40), so by
that formula the long tier would never pay. It pays because the short tier does not get
the hit **at all** when the gap exceeds 5 minutes: the next request re-sends the prefix at
¥20 instead of ¥2. So the honest comparison is between `n'` *re-sends the short tier
would have missed*, each saving `input − hit = ¥18`, against the extra `¥20` write — that
is Kimi's own arithmetic ("命中 1 次，省 ¥18；命中 2 次，共省 ¥36，扣掉多付的 ¥20，净省 ¥16"),
and it is why the same prefix reused twice within the hour already pays.

The same shape answers the other two:

- **Alibaba explicit vs implicit** (qwen3.8-max): write 15 vs 12 and hit 1.0 vs 1.5 — the
  1.25×/10% general rule does not hold for these models, they are the official exceptions.
  Extra write ¥3 per token layer, saving ¥0.5 per hit ⇒ break-even at **6 hits**.
- **Volcengine explicit vs implicit** (doubao 2.1-pro): the hit rate is identical (¥1.2),
  so nothing is saved per hit; what is bought is determinism, and what is paid is storage.
  One million cached tokens for the full seven days is 168 hours × 0.017 = **¥2.86** —
  less than one full-price re-send of the same prefix (¥6). Cheap insurance, not a saving.

## What this server buys today

It used to send **no cache directive**. It now sends one tier per session on every
request: the session's own `cache_tier` if it named one, otherwise the shortest
tier the roster declares. `cache_ttl_tiers` is the only list it may choose from.
A session that asked for `cache_keepalive` gets a `(time notice: ...)` turn each
time that tier's window is nearly spent, and the chain has no last one; tool calls
are interrupted before the window ends rather than priced after it. The affinity
key and `X-Session-ID` are separate opt-in switches. See
`docs/notes/server-commands.md`. The table below describes what each tier costs
and what a provider leaves on the table, not a per-model decision this server makes:

| provider | tier in force | paid | left on the table |
| --- | --- | --- | --- |
| kimi-coding | implicit **5m** | write (1× input) + hit | **1h, reachable on the surface we speak** (`prompt_cache_options`); every gap over five minutes is a full-price re-send |
| bytedance-ark | implicit | hit; **no storage fee** | `prompt_cache_key` affinity (free); the explicit lease (determinism, needs `previous_response_id`) |
| qwen-token-plan, qwen-dashscope | implicit | hit at 12.5% | explicit needs >6 hits per layer to pay, and `cache_control` on a surface we do not speak |
| tencent-tokenhub | implicit | hit | `prompt_cache_key` + `X-Session-ID` (free) |
| minimax-cn | **anthropic face, 5m explicit** (since 2026-11-05) | hit; no write fee | nothing on that face -- but note the OpenAI faces below |
| deepseek, stepfun, longcat, dots, zhipu, local | the only tier there is | hit | nothing to choose |

One reading of MiniMax changed the design on 2026-11-05, and it is worth keeping beside
these tables because it is a distinction the tables cannot show. Its OpenAI-shaped faces
do **not** serve a prefix: a new prompt answers 128–192 cached however identical its head
is to a prompt already stored (a real 19,422-token body: 19,421 for the prompt sent the
day before, **0** for a sibling whose card was renamed, 128 for a one-character change).
Its `/anthropic` face does: the same 8,260-token system text with a `cache_control`
breakpoint answered 8,192 across a changed user message, and 8,266 with a sentence
appended. So "it caches" is not a property of the vendor but of the face a request is
sent to, and the roster now says which face each provider is spoken in. See
`docs/notes/server-anthropic.md`.

What the server already reads back: `usage.input_tokens_details.cached_tokens`, which is
the one measurement that could correct any of this. Kimi also reports
`cache_write_tokens`, and per-TTL write counts on the Anthropic surface; neither is read.

## Request planning per mode — what it would take

Planning the *request* (not just the emit order) means choosing, per model and per
request, which tier to ask for. The shape that fits this repository without inventing
forward compatibility:

**What the roster would have to state, per tier** (it states the first two today):

| fact | today | needed |
| --- | --- | --- |
| which tier is the default, and its length | `cache_ttl_tier`, `cache_ttl_secs` | ✔ |
| the other lengths available | `cache_ttl_tiers` | ✔ |
| renewal semantics | prose in `cache_ttl_note` | a word: `renewing` / `deadline` / `evicted` |
| the wire field and value that selects a tier | absent | e.g. `prompt_cache_options.ttl = "1h"`, on which surface |
| write / hit / storage price for that tier | partly (`cache_write_5m`, `cache_write_1h`, `cache_storage`) | keyed per tier |
| minimum cacheable tokens | prose | a number (1024 default, 2048 / 256 / 128 / 8960 for named models) |

**What the server would have to do:**

1. `Rates` becomes per tier, not per model: `(ttl_ms, renews: bool, write, hit, storage,
   minimum)`; `price.miss_ratio()` generalizes to "cost of this request under this tier".
2. `llm` learns to carry the field, so that a tier can actually be requested; a tier with
   no wire field is a tier that cannot be planned for.
3. `expire` learns the deadline semantics: a node records when it was *written* as well as
   when it was last sent, and a non-renewing tier expires from the former.
4. The choice becomes an expectation over the *gap distribution*: the same session has
   intra-turn gaps (seconds) and inter-turn gaps (minutes to hours), so a tier is worth its
   extra write only if enough of the next requests fall inside its window. `cached_tokens`
   from `usage` is the measurement that closes the loop — per model, per tier — and turns
   `cache_invalid_rate` from a machine-wide hyperparameter into an observed number.
5. Route affinity (`prompt_cache_key`) is planned separately from the tier: it is free,
   applies to implicit caches, and its value must be stable per conversation (a session
   name is a reasonable seed, an unrelated random id is not).

**Invariants a planner must keep** (the reason this is written down before it is built):

- A plan may only price a **tier it can request**. Planning for 1h on a surface that
  ignores `prompt_cache_options` silently plans for 5m.
- A tier's window is a claim about the provider, and the plan's answer to "is this chain
  warm" is only as good as that claim; `cached_tokens` is what proves it, and a plan that
  never looks at it is a guess with a receipt.
- Choosing a longer tier never reduces hit rate; it only moves cost from re-sends into
  writes and storage. So the decision is a comparison of expected costs, never a
  preference for the longest TTL.
- A deadline tier's chain dies on a clock nobody can reset; a renewing tier's does not.
  The two must not share an expiry function.

## Unverified

- Whether Kimi charges the write on newly written tokens only (inferred from its "总价不变"
  note and Alibaba's literal statement; not measured here).
- Whether `prompt_cache_options` is honoured on Kimi's Responses surface for every model
  the roster lists (the doc says Chat Completions and Responses alike; not sent yet).
- Whether a `prompt_cache_key` on a session name measurably raises the hit rate on
  Volcengine or Tencent (both call it best-effort).
