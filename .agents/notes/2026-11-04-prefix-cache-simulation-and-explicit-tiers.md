# 让 prefix cache 真实模拟、并按策略调用显式缓存 API（WIP）

2026-11-04。User 的指令：「阅读该项目关于各家 LLM 厂商的缓存计费模型，思考如何让
Prefix Cache 系统能真实地模拟并且使用策略调用显式缓存 API」→ 第二轮「记，然后开工」。

工作树：`.worktrees/cache-tiers`，分支 `feature/cache-tiers`。主 checkout 当时有
`.agents/skills/evolve/SKILL.md`、`README.md`、`SPEC.md` 三处不属于本轮的未提交改动，
所以按 whiting-dev 的 worktree 规矩另开，不碰它们。

## 读了什么

- `docs/reference/prefix-cache.md`（2026-11-03 提交 `5d8c599`，唯一讲清厂商事实的文档）
- `~/.agents/gongga-agents/providers.toml` 头部 1–94 行（字段契约 + 哪些字段服务端不读）
- `docs/notes/server-prefix-cache.md`、`crates/server/src/prefix_cache.rs`
- `.agents/notes/2026-11-03-roster-facts-nothing-reads.md`（第四节：唯一的实测被丢掉）
- `.agents/notes/2026-11-03-skill-table-in-cache-order.md`（第八轮：真实缓存形状）

## 计费模型：三个正交的问题

1. **谁决定缓存什么**：implicit / explicit marker（`cache_control`）/ explicit lease
   （`store:true` + `caching` + `expire_at` + `previous_response_id`）。
2. **活多久、命中是否续期**：renewing / deadline / evicted。火山方舟显式档是
   deadline，官方明说「不会随着缓存 / 存储的使用而重置缓存生命周期」。
3. **怎么收钱**：`(input−cached)×input + cached×hit + 新增写入×write + 存储×小时 + output`。
   hit 折扣从免费（dots3）到 120×（mimo）；write 有 1× / 1.25× / 2× 三种；storage 只有
   方舟 0.017 与智谱 0.00 公布。

真正决定分档收益的不是单价差，而是**短档在 gap 超窗时整段按全价重发**。Kimi 5m 与 1h
的 hit 同为 ¥2，长档唯一的价值是少几次 ¥18 的全价重发；文档给的例子是命中 2 次即净省
¥16。所以策略必须是对 gap 分布的期望成本比较，不是「选最长 TTL」。

## 四个落差（本轮判断的根据）

1. **两个模拟器都不真。** `prefix_cache::Rates` 只压了 `input/cache_read` 比值、全机超参
   `cache_invalid_rate`、单一 renewing TTL；`scripts/fake_llm.py::measure()` 按 unit
   逐字节比对上一次请求并 `hit//4`。但第八轮实测已经推翻后者：真 provider 是**整条请求的
   token 前缀、按 128 token 块量化**，不是 unit 序列。fake 现在报的 cached% 用的是被证伪
   的旧模型 —— 「真实地模拟」第一步是把它换掉。
2. **唯一的实测被丢掉。** `usage.cached_tokens` 只进 `events` 日志、TUI 卡片、
   `sessions.cached_tokens`，从不回灌规划。`Price` 只读 `input/cache_read/output`，
   `cache_write*` / `cache_read_explicit` / `cache_storage` / `band` 全不读，四元公式在
   代码里没有落点。
3. **Responses 面上能真调用的显式档只有 Kimi。** `llm::build_head` 硬编码 `/responses`；
   阿里 / MiniMax 的 `cache_control` 在 Chat Completions / Anthropic 面；火山方舟的 lease
   要 `store:true` + `previous_response_id` 链式 + 空 `instructions`，与本机
   `store=false`（`config.rs:439`）+ 每轮全量 transcript + `instructions` 的形状**正好相反**。
   免费可拿的只剩 `prompt_cache_key` / `X-Session-ID`。不改请求形状的话，策略空间就是
   Kimi `prompt_cache_options` 的 5m ↔ 1h 两档。
4. **两条策略不在同一时刻。** `plan()` 只在首轮 `index_of` 生效（index 一次写定），
   `confirm()` 在 `Completed`；档位决策必须每请求做。`Node::touched` 只有一个标量，
   同一条链在 5m 档已冷、在 1h 档还热表达不了。把 tier 塞进 `plan()` 是错层。

## 一处自我修正（上一轮的说法要改）

上一轮我把 L1 的 `expected_prefix_tokens` 说成「来自 `plan` 选中链的尺寸」。**这个不对**，
两处：

- `Plan` 根本不暴露它，要拿就得改 `prefix_cache` 的返回类型；
- 更要紧的是 chain size 只覆盖 index 条目，而 `cached_tokens` 数的是整条请求
  （散文 + tools + transcript）。分母比分子小一截，命中率会 >1，后验直接失真。

更稳的分母是**同一会话上一轮 `usage.input_tokens`**：transcript 是 append-only，
prompt 行首轮写定后原样重放（第七轮），所以连续两轮之间理论最大命中就是上一轮的输入
token 数。`hit_ratio = cached / prev_input` 不需要任何假设，128-token 量化造成的偏差
本身可测。由此 L1 观测层可以**完全不碰 `prefix_cache.rs`**，只在 `agent.rs` 的
`Completed` 里落一行 —— A 的改动面因此小很多。

## 选定的切口：C → A

文档自己列了三条 Unverified（Kimi 是否只按新增收 write、`prompt_cache_options` 在
Responses 是否对每个模型生效、`prompt_cache_key` 是否真提升命中率）。直接把这些数字
写进策略，就是文档里那句 "a guess with a receipt"。所以先证伪、再落地观测。

- **C**：`scripts/cache_probe.py` 真请求回答三问 —— ① `prompt_cache_options` 在
  Responses 上是否被接受且改变 TTL 行为；② write 是否只收新增；③ gap>5m 时 1h 档
  是否真命中。结果落 `.dev/verify/`。
- **A**：L1 观测层（`Usage` 读 `cache_write_tokens`；每次 Completed 记
  `model/tier/session/directive/gap_ms/expected/cached/write`；按 model+tier+gap
  汇总后验）+ 免费 affinity（`prompt_cache_key` / `X-Session-ID`）+ Kimi 档位做成
  可手动指定。不自动选档、不触发任何写入费。
- **B（全套 L0–L4）暂不做**：会跨 `providers/config/prefix_cache/llm/agent/store`
  + roster schema（社区仓要发 PR）+ ship config，且方舟/阿里那两档会逼出「要不要换
  surface、要不要放弃 `store=false`」的架构决定。等拿到 `cached_tokens` 后验再定。

## 不变式（不要顺手改）

- 只价格**能发出 wire 字段**的档；规划 1h 却发不出 `prompt_cache_options` 就是静默
  按 5m 计费。
- deadline 档的过期从**写入时刻**算，renewing 档从 `touched` 算；两者不能共用一个
  `expire`。
- 发过的内容不改写（第七轮），index 首轮写定。
- 不写测试（AGENTS.md / whiting-dev），不改 Invariant/Safety，不改 SPEC.md。

## 本轮结果：C 的三问，两问有答案，第三问推翻了前提

`scripts/cache_probe.py`，真请求、真 key（`KIMI_CODING_API_KEY`），数据在
`.dev/verify/cache-probe-*.json`。三次运行共 16 次请求，最长的等了 330 秒。

**Q1 `prompt_cache_options` 在 Responses 上生效吗 —— 生效，但只在 coding 面被测过。**

- 一开始的担心是 surface 对不上：roster 里 `kimi-coding` 只声明了
  `openai-completions` 和 `anthropic-messages`。实测 `/responses` 返回 **200**，
  回包带 `prompt_cache_key` / `prompt_cache_options` / `service_tier` /
  `previous_response_id` 这些 Responses 原生字段。
- 带 `{"mode":"implicit","ttl":"1h"}` 的请求，回包 `body_keys` 里出现
  `prompt_cache_options`；不带则没有。
- 判别实验：`5m` / `1h` → 200 并回显；`bogus` / `300` → **400**，错误信息点名
  `prompt_cache_options.ttl`。所以这个字段被**校验**，不是透明 echo。
- 注意：这只证明 coding 面（`api.kimi.com/coding/v1`）接受它。文档引用的是
  `platform.kimi.com` 的开放平台文档，两者是 roster 明说「互不通用计费」的产品，
  不能相互外推。

**Q2 write 是否只收新增 —— usage 层无法直接验，但拿到了正确的计费基数。**

- `kimi-for-coding`（K2 系）与 `k3.1-pro`（K3 系）上，
  `input_tokens_details.cache_write_tokens` **恒为 0**，连冷请求明明写入了
  1502 token 也是 0。文档说「Kimi 也报 cache_write_tokens」——这个字段在
  Responses 面上没有信息量，不要拿它核算写入费。
- K3 的四步（`.dev/verify/cache-probe-20261104-140056.json`）：

  | 步骤 | input | cached | 未命中 |
  | --- | --- | --- | --- |
  | cold | 1502 | 0 | 1502 |
  | repeat | 1502 | 1502 | 0 |
  | extended | 1785 | 1280 | 505 |
  | extended_repeat | 1785 | 1785 | 0 |

  扩展请求只让新增段未命中，与官方「仅对新增部分计费」一致；但 1h 档 2×
  的写入单价只体现在账单里，usage 看不见，所以这一条仍是**推断**。
- 可用的写入计费基数是 `input_tokens − cached_tokens`，不是那个字段。
- 附带证实上一轮的修正：`repeat` 的 cached 精确等于**上一轮 input_tokens**
  （1502 → 1502、1785 → 1785）。`hit_ratio = cached / prev_input` 这个分母
  在稳态等于 1.0，不是估的。

**Q3 gap>5m 时 1h 是否真命中 —— 两档都命中，文档的前提被证伪。**

`.dev/verify/cache-probe-20261104-140828.json`：两档各自冷写一次，共享等 330 秒
（>5m），再各发一次同样的前缀：

| tier | cold cached | after 330s cached |
| --- | --- | --- |
| 5m | 0 | **1505** |
| 1h | 0 | **1505** |

短档没有像文档和 roster 说的那样在 5 分钟后失效、整段按全价重发。上一条判别
实验排除了「字段没生效」这个解释。可能的解释：TTL 是懒惰清理 / 实际窗口比
宣称长 / 「命中即续期」之外还有别的保留机制 —— **本轮没有区分它们**。

对策略的影响很直接：L3 那条 `P(短档超窗) × P × (input − hit)` 是整套分档
算账里最大的一项，而它的概率在这把 key 上没被观测到。在这条被量化之前，
**不要实现自动选档**；这正是选 C→A 而不是 B 的理由，现在有了实测依据。

## 本轮落下的代码（A 的观测层 + 可手动指定的档）

- `crates/server/src/llm.rs`：`Usage::uncached_tokens()` = `input − cached`；
  `InputTokens` 读 `cache_write_tokens`（记录但不参与决策）；`LlmClient` 带
  `cache_tier` / `cache_affinity`，`build_head` 注入 `prompt_cache_options`
  与 `prompt_cache_key`。档位先过 roster 的 `cache_ttl_tiers` 过滤。
- `crates/server/src/providers.rs`：`cache_ttl_tiers` 从「服务端不读」变成
  `FileProvider → Provider → Entry` 的一等字段。
- `crates/server/src/config.rs`：`[model] cache_tier` / `cache_affinity`
  （环境变量 `GONGGA_CACHE_TIER` / `GONGGA_CACHE_AFFINITY`），`for_entry`
  按 provider 带过去。affinity 默认 **false**：不是每个 endpoint 都认这个字段。
- `crates/server/src/store.rs`：`cache_hit_posterior(model)` 用 `json_extract`
  聚合已有 `events` 表的 `usage` 行（复用现成的轮子，没造新表）；
  `last_usage_at(session)` 给 gap。
- `crates/server/src/agent.rs`：`usage` 事件补 `expected` / `uncached` /
  `hit_ratio` / `gap_ms` / `cache_tier` / `cache_write`；`rates()` 有实测行时
  用后验覆盖 `cache_invalid_rate`，没有才用配置值。`scale()` 的公式没动。

## 没做 / 下一步

- `X-Session-ID` header（Tencent 那一条 affinity）没接：要动 `stream_once`
  的 header 构造，且需要 roster 声明哪家接受。
- `ship/config/config.toml` 里 `cache_invalid_rate` 的注释仍是错的（说可以按
  provider 调，其实是全机一个概率）。改 ship 会触发发布流程，留到下次动 ship。
- L2 模拟器还没改：`fake_llm.py::measure()` 仍是被证伪的 unit-by-unit 模型。
- L0 结构化 tier（wire field / renewal / write / storage / minimum）没做，
  L3 自动策略按上面的 Q3 结论**暂缓**。
- Q3 的对照实验还欠一次：不带 directive 的冷请求同样等 330 秒，用来区分
  「ttl 字段没改变保留期」与「默认档本来就超过 5 分钟」。

## 门禁（本轮真实执行，退出码是契约）

| 门 | 结果 | 说明 |
| --- | --- | --- |
| `cargo build -p gongga-server -p gongga-cli -p gongga-tui` | exit 0 | 用 worktree 默认 `target/`。第一次用独立 `.dev/target-cache-tiers` 撞 Windows `os error 5`（AGENTS.md 记过的并发 exe 锁），换默认 target 后通过 |
| `cargo check -p gongga-server` | exit 0 | |
| `cargo fmt --all -- --check` | 0 | 先跑过 `cargo fmt --all` |
| `comments-check` | 13 violations | **与主 checkout HEAD `99355ea` 的基线逐行一致**。初跑 23 条，多出的 10 条全在我改的五个文件里；把新注释压到 120 字符内后回到 13，没有扩大既有违规 |
| `test-layout-check` | 0 violations | |
| `scripts/skill_contract.sh` | PASS 8 skills | 在主 checkout 跑。worktree 没初始化 `ship` 子模块，那里只报 1 skill，等于空跑，不算数 |
| `scripts/ship.sh --check` | exit 0 `in agreement` | 同样在主 checkout 跑；worktree 里那次输出 `no ship/ here`，是跳过 |
| L3 `scripts/prompt_selftest.sh` | `PASS · prompt · 99355ea` | 真服务端 + fake LLM。`FAKE_LLM_DUMP` 里 2 个请求都带 `prompt_cache_key="reader"`；`prompt_cache_options` 按设计缺席（fake roster 没声明任何 tier，过滤器正确挡住） |

`cargo build --workspace` 在这台机器上没跑通：`crates/bundle/build.rs` 把嵌套
target 写死成 `../../target/bundle-payload`，与主 checkout 的 exe 锁相撞
（`os error 5`）。不是本轮改动引入，没有绕开、没有改 build.rs，如实记在这里。
`prompt_cache_options` 的服务端注入没有独立的 L3 断言：真 provider 那一侧由
`cache_probe.py` 验证字段被接受，服务端这一侧靠 `cargo check` + L3 里
`prompt_cache_key` 走的是同一条 `build_head` 路径。

测试 home 与它 spawn 的进程都已收干净（按 pid 查过，无残留）。

