# 标题印着 `thinking high`，请求里没有 `reasoning` —— 一次对账

日期：2026-11-01

## 症状（用户原话）

「出现了 TUI 显示（我设置的 model, thinking effort）和实际请求不同的情况，需要查明真相。」

## 结论：标题里的档位不是请求里的档位

两条路读的不是同一个字段：

| 谁 | 读什么 | 代码 |
| --- | --- | --- |
| 窗格标题 | `tuning.thinking_effort`，**没有就用名单里该模型的 `reasoning_default`** | `commands.rs` `session_info` |
| 请求体 | 只在 `effort.is_some()` 时写 `reasoning.effort`；`effort` 只来自 `Tuning::thinking_effort` | `agent.rs` `turn_inner` → `llm.rs` `build_head` |

所以**没显式设过档位**的会话：标题照样印一个档（名单推荐值），请求一个字节都没带，
真正生效的是厂商自己的默认 —— 名单那个值没有任何东西去执行。

本机 12 个会话 `sessions.thinking_effort` **全部是 NULL**，其中 11 个的模型在名单里带
`reasoning_default`：

| 会话 | 标题会印 | 请求带的 |
| --- | --- | --- |
| fix_model / help_HR / choir / model_pooling / tui | thinking high | （无） |
| polity_test / polity-paste-image-HR | thinking max | （无） |
| polity / tui_fix / remote | thinking high | （无） |
| polity-paste-image-premier | thinking off | （无） |
| polity-paste-image-advisor（k3 无默认） | （不印） | （无） |

## 三次测量，不是读代码

1. **库里的行**：见上表，`thinking_effort` 全 NULL，`model` 与标题一致。
2. **假端点 + 临时 home + 临时名单**（`scripts/fake_llm.py` 的 `FAKE_LLM_DUMP`），
   名单里给模型 `reasoning_default = "high"`：
   - 建会话不设档位 → body 里**没有 `reasoning` 键**；
   - `set_session --thinking_effort medium` → 名单 `reasoning_mapping` 映射成 `high`
     → 下一条 body `"reasoning": {"effort": "high"}`。
   即：只有显式设过的东西才上得了线。
3. **真 TUI**（`scripts/tui_live.py`，ConPTY 抓帧）：`/set_session --session main
   --model deepseek-official/deepseek-flash` 之后帧里是
   `[main] cwd … · model deepseek-official/deepseek-flash · thinking high · …`，
   而同一个会话的行是 `('main', 'deepseek-official/deepseek-flash', None)`。

## model 那一半：一致（只有两个短暂窗口）

标题是 `provider/model_name`（名单拼的），请求 body 的 `model` 是 `model_name`，
同一个模型，只差前缀。能让两者不一致的只有：

- **一轮在飞时改设置**：`route` 和 `effort` 在每个 turn 开头各读一次
  （`agent.rs` `turn_inner`：「Resolved once: every step of a turn sends to the model it
  started on」），而 `set_session` 立刻 `publish_sessions` —— 标题马上变，那一轮照旧发旧的。
  这是「标题说的是这轮会发什么」而不是「在发什么」。
- **名单看得见、请求 resolve 不了**：标题走 `place()`（不要求有 key），请求走 `resolve()`（要求）。
  provider 没 key / 钉在 `retired_ids` 上的会话，标题写得出模型名，那一轮直接报错、不发请求。

## 记录缺口：这件事事后查不出来

- `turn start` 只记 `model` 和 `budget`，不记档位（`agent.rs`）。
- `llm_attempt` 探针（`probe.rs`）里没有 model / effort。

所以「TUI 显示的和实际请求不同」这种话，从服务端记录里无法证实也无法证伪 —— 只有把
`effort` 补进日志/探针，才有下一次可以对照的东西。

## 另外两个「设了却不生效」的口子（同一次排查顺手发现的）

- `set_sticky --name thinking_effort --value high` **静默无效**：`Sticky::fill` 只填
  `required` 或声明 `sticky` 的参数，而 `set_session --thinking_effort` 两者都不是；
  `Sticky::set` 又不校验名字，于是存下来一条永远没人读的粘性参数。
- 改 `--model` 时旧档位**不重新校验**：档位是在设的那一刻按当时的模型校验并映射的，
  之后换模型，旧值原样发出去（对名单里没有该档位的模型可能直接 400）。

## 用户的裁决：C（2026-11-01）

「建会话时落成设置」：把名单默认在 `set_session` 写的那一刻就落成会话自己的档位，
标题只报行里的东西。已实现：

- `Entry::level_in_force(stored)`（`providers.rs`）：存的档位这个模型还收就留着，
  否则换成它的 `reasoning_default`，且**只在这份名单真的公布了该档位时**才认 ——
  `MiniMax-M3` 的 `reasoning_default = "off"` 是模式不是档位，它没有 `reasoning_levels`，
  所以答案是不发。
- `set_session`（`commands.rs`）：`--thinking_effort` 给的走原来的映射+校验；
  没给的走 `level_in_force`，**变化才写**。于是：新建会话、换模型都会得到一个档位；
  只改 `--context_limit` 保留原档位；换到不收旧档位的模型则换成新模型的默认（顺手补上
  之前「旧档位原样发出去」那个口子）。
- `session_info`：删掉 `reasoning_default` 回退。标题报的就是行里的值，请求读的也是它。
- `docs/notes/`：`server-commands.md`（新增 `LEVEL_IN_FORCE`）、`server-session.md`、
  `server-providers.md`、`cli-lib.md`、`protocol-message.md` 同步。

## 验证（这一次两边都看得见）

临时 home + 临时名单（三个形状：`probe-thinky` 默认 high、`probe-quiet` level 只到 low/high、
`probe-nolevels` 只有 `reasoning_default = "off"` 没有 levels）+ `scripts/fake_llm.py` 的
`FAKE_LLM_DUMP`：

| 命令 | 落盘的档位 | 请求里的 `reasoning` |
| --- | --- | --- |
| `--model probe/probe-thinky`（不点档位，新建） | `high` | `{"effort":"high"}` ← 改动前这里是**没有这个键** |
| `--context_limit 40`（同模型） | `high` | — |
| `--model probe/probe-quiet`（旧值 high 它收） | `high` | — |
| `--model probe/probe-nolevels` | 无 | 没有 `reasoning` 键 |
| thinky 上存 `none`，再换 `probe-quiet`（不收 none） | `low` | — |
| `--thinking_effort ultra` | 报错，列出可用的档位 | — |

真 TUI（`scripts/tui_live.py` 抓帧）：行里有档位时标题是
`[main] … · model deepseek-official/deepseek-flash · thinking high · ctx_limit 70%`；
把同一行改成 `NULL` 再开窗，标题变成 `… · model deepseek-official/deepseek-flash · ctx_limit 70%`
—— 不再出现「标题有档位、请求没有」的组合。

（对账时顺手踩到一个坑：`tui_live.py` 起的服务端只把**服务端自己的** base_url 指向假端点，
会话 `--model` 用的是真名单条目，于是那一次探针真的打到了 api.deepseek.com 并花了 2251 输入 /
100 输出 tokens。请求体的证据必须用临时名单，不能用真名单。）

## 老行仍然没有档位（C 明说的代价）

这次改动只影响从今往后被 `set_session` 写过的会话。库里已有的行（本机 12 个全是 NULL）
标题不再印档位、请求也不带 —— **两者一致，只是没有信息**。谁给它跑一次任何参数的
`set_session`，它就落到当时模型的默认上。想给老会话一次性补齐，得在加载行时把默认落盘
（`materialise_locked` 或一次迁移），那会**改变老会话实际发出去的东西和花费**，没有用户
点头不做。

## 模型名字那一半：polity_test 就是 kimi-for-coding

用户追问：「session polity_test 显示他是 kimi-for-coding，但他看了服务器说自己是 k3」。

- 行：`model = kimi-coding/kimi-for-coding`、`model_name = kimi-for-coding`、档位 NULL。
- 服务端日志：`turn start session=polity_test model=kimi-coding/kimi-for-coding` **12/12 次**，
  一次 k3 都没有。
- 请求体的 `model` 是 `model_name`（`for_entry` → `cfg.model_name = entry.model_name`），
  即 `kimi-for-coding`；标题是 `provider/model_name`，同一个模型的两种写法。

「k3」的来源查到了：`polity_test` 的 seq 104 是它自己跑
`model_pool pick --tier flash/sota/pro` 的输出，三行分别是 `stepfun/step-3.5-flash`、
`kimi-coding/k3`、`kimi-coding/kimi-for-coding` —— 它把菜单里的 k3 记成了自己的模型。
名单里 `kimi-coding/k3` 确实存在，只是这个会话从来没被设成它。

根因是**协议里没有任何东西告诉一个会话它自己跑在哪**：`Hello.model` 是服务端自己的模型
（`config [model]`），`cli.toml` 的 sticky `model` 是客户端上次点的东西，两者都不是答案。
今天唯一能问到的读法是 `gongga-cli set_session --session <name>`（不带别的参数：不改任何设置，
`data.model` / `data.thinking_effort` 就是答案）。要不要一个只读的、明说的命令，待用户定。

## 追加：设了 max 还是不思考 —— 是 `.done` 被丢掉（2026-11-01 第二轮）

用户第二次报告：「即便我在 TUI 设置了 thinking max，session 还是不思考」。

查下来与档位无关，是 **reasoning 文本在流里换了形状**，而服务端只认其中一种。

### 证据（全部来自本机已有的 `events` 记录，不花额度）

按 `llm_attempt` 的 `event_kinds` 分桶：

| 会话 | 形状 | 次数 |
| --- | --- | --- |
| tui_fix（kimi k3.1-pro） | `reasoning_summary_text.delta` + `..._done` | 54 |
| tui_fix | **只有 `..._done`，没有 delta** | 128 |
| polity_test（kimi kimi-for-coding） | delta + done | 6 |
| polity_test | **只有 `..._done`** | 100 |
| fix_model（deepseek-flash） | `reasoning_text.delta`（另一套事件名） | 125 |
| polity-paste-image-premier（MiniMax-M3） | 一个都没有 | 162 |

于是：kimi 大多数请求只发 `response.reasoning_summary_text.done`（整段文本一次给），
`output_item.done` 的 `summary` 是空数组；服务端 `handle` 只处理 `*.delta` 与
`output_item.done`，`*.done` 落到 `_ => {}`。结果 `summary` 空 → 前端没有东西可画，
落盘的行是 `{"summary":""}` —— 看起来就是「没思考」。MiniMax-M3 那次是另一回事
（它自己不发 reasoning，与 roster 里 `reasoning_default = "off"` 一致）。

### 改了什么

- `llm.rs` `handle`：新增 `response.reasoning_summary_text.done` /
  `response.reasoning_text.done` 分支。`done` 按定义是全文，所以条目已累积的文本是它的**前缀**，
  只有余下部分是新的：一条规则同时覆盖「只发 done」（整段当作一个 delta 发出去）和
  「delta + done」（不重复），以及「done 与自己的 delta 不一致」（保留流过的，不显示第二份）。
- `probe.rs`：`reasoning_chars` 也认 done 文本（仅在该次尝试还没有 delta 时），否则一次
  「整段给」的运行会被记成 0 字符思考。`effort` 也进了探针 payload。
- `agent.rs`：`turn start` 那行日志加 `effort=`，让「标题说的档位 vs 请求带的档位」以后一条
  查询就能对账（这是上一轮记的记录缺口）。

### 验证

临时 home + 临时名单 + 一个只按 `REASONING_SHAPE` 发声的假端点（`done` / `deltas` / `both` /
`none`），一次服务端生命周期跑完一个场景，读库里的 `display.summary` 与探针：

| 形状 | 落盘 summary | 探针 |
| --- | --- | --- |
| `done` | `"first paragraph of thought\nsecond paragraph of thought"` | deltas 0, chars 54 |
| `deltas` | 同上 | deltas 2, chars 54 |
| `both` | 同上（**无重复**） | deltas 2, chars 54 |
| `none` | （没有 reasoning 行） | deltas 0, chars 0 |
| 变异检查：`done` 分支不读文本 | `""`（正是用户看到的症状） | deltas 0, chars 54 |

重放侧也验了：`gongga-cli get_messages --session s --range -5:` 现在给客户端一行
`{"t":"response","part":"reasoning","text":"first paragraph of thought\nsecond paragraph of thought","done":true}`
—— 与实时 delta 落到前端的是同一个事件，只是 `done` 为真。

### 教训（又一条）

`tui_live.py` 起的服务端只把**服务端自己的** base_url 指向假端点；用 `/set_session` 带上一个
真名单模型，那一轮就真的打到厂商。这次又踩了一次（一次 turn），所以「端到端假端点」必须
配假名单（`GONGGA_AGENTS_DIR`），这是唯一安全的方式。
