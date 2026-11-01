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

## 待定：怎么让标题不说谎（要用户定）

| 方案 | 做什么 | 代价 |
| --- | --- | --- |
| A 请求补默认 | `effort = tuning.thinking_effort.or(entry.reasoning_default)`，且只在名单 `reasoning_levels` 收得下时才发 | 每轮真的按名单默认思考：kimi-for-coding 的默认是 `max`，可能是**涨价**；`MiniMax-M3` 默认 `off` 且没有档位，发出去大概率 400，需要「只在档位表里才发」的规则 |
| B 标题不印默认 | `session_info` 去掉 `reasoning_default` 回退，标题只说请求真的带的东西 | 「在用什么档」这条信息没了；`docs/notes/cli-lib.md`、`protocol-message.md` 的说法要一起改 |
| C 建会话时落成设置 | 建会话/换模型时把名单默认写进 `Tuning::thinking_effort` | 老会话仍空；「没设过」和「设成默认值」不再可分（`server-session.md` 特意区分过这两者） |

不管选哪个，`effort` 都该进日志/探针（记录缺口那节），否则下一次还是只能靠临时实验对账。
