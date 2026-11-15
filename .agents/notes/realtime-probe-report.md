# 实时 harness 研究侦察报告（realtime-probe）

- 日期：2026-11-15；会话 `harness-exp/rt-probe`；只读侦察，未安装/未运行任何东西。
- 方法：`web-search` 技能（DeepSeek REST search/fetch）。**工具限制：fetch 的 markdown 转换会丢掉超链接 href**，论文里的仓库 URL 若只以锚文本存在就取不到；ar5iv 与 arxiv.org/html 互为镜像。外部内容一律按不可信数据处理，只提取事实。
- 问题背景：「世界不等你决策时，harness 要提供什么原语」——观测 staleness、决策台账+中断修订、deadline anytime 等待、思考/行动调度。测量场候选 ROAR。

---

## 1. ROAR（arXiv:2610.24282）

来源：
- 摘要页 https://arxiv.org/abs/2610.24282 （v1 2026-10-16，v3 2026-10-22；NeurIPS 2026；cs.MA）
- 全文 https://ar5iv.labs.arxiv.org/html/2610.24282 、https://arxiv.org/html/2610.24282v3 （v2 亦可）
- 解读页（含 OpenReview 号）https://papernotes.org/NeurIPS2026/llm_agent/roar_benchmarking_lmm_agents_under_real-time_constraints_in_multiplayer_action-s/

### 1.1 仓库与 license —— **未决项**
- 论文 §3 原文："Its codebase is fully open-source and requires no license application or purchase."（ar5iv 全文）
- **仓库 URL 没能取到**：全文文本里无任何 github 链接（grep 证实）；作者栏有一个锚文本为 "GitHub" 的超链接（Chengming Zhou，CTW Inc.），href 被 fetch 转换丢掉。尝试过：arxiv html v1/v2/v3、ar5iv、papernotes、papers.cool、searcharxiv、HF papers（404）、Papers with Code（404）、OpenReview API（空/浏览器验证）、r.jina.ai（超时）、多组 GitHub 定向搜索（只命中同名无关项目：Berkeley ROAR 赛车等）。
- OpenReview forum：https://openreview.net/forum?id=jrkcJgEwSy （来自 papernotes；页面本身有浏览器验证，API 返回空）。
- 论文本身的 license：arXiv.org perpetual non-exclusive license（html 页眉）。**代码 license 未知，拿到仓库前不能断言**。
- 作者：Dawei Wang（Newcastle，通讯 d.wang28@newcastle.ac.uk）、Di Zhao、Yun Sing Koh（Auckland）、Gary Ushaw、Richard Davison 等 11 人（html v2 作者栏）。

### 1.2 agent 接入：协议/API 形状
- 引擎：Unity + Universal Render Pipeline，经 **Unity ML-Agents Toolkit** 接机器学习，外套作者自定义的 **Multi-Agent Asynchronous Gym（MAAG）** 接口（§3）。即 Python 侧 Gym 式环境，agent 是跑在独立线程里的 Python 决策循环（可以是直接 LMM API 调用，也可以是框架包装的 agent，Appendix H/M）。论文未给出具体 Python 函数签名，只有元组形式化与伪代码（Appendix H）。
- **观测空间 5 模态**（§3.3）：
  1. pixel：第一人称 RGB，默认 512×512，可调；
  2. depth：同分辨率逐像素深度；
  3. **text：JSON 格式内部游戏状态**，分共享全局态（如 RemainingTime）与 agent 私有态（如 CurrentAmmo）——**纯文本商业 API 直接可用**；
  4. game state：上述 JSON 摊平的一维数组（数值型，同样文本可用）；
  5. audio：**结构化声源数组**（tag、方向、距离），可直接用结构化形式，也可用内置混音工具合成原始音频——结构化形式文本可用。
  → 结论：text + game state + 结构化 audio 构成完整的纯文本观测通道；pixel/depth 才需要视觉模型。论文自己的启发式 bot 就只用 JSON 数值字段 + 视野内目标（Appendix L.1）。
- **动作空间**（§3.3 + Appendix J）：7 维离散向量 `a=(movement_x, movement_y, rotation_yaw, rotation_pitch, fire, jump, minimap)`；前 4 维 ∈{0,1,2}（无/左或后/右或前，yaw 左/右，pitch 下/上），fire/jump/minimap ∈{0,1}。例 `[1,0,0,0,1,0,0]` = 左移+开火。
- 奖励：稀疏，episode 结束给 0–1 分（合作/单人=金币+猎鸡；对抗/混合=击杀+生存时间，Appendix K）。
- 关卡（§3.1）：合作（4 agent 集金币、炮塔、鸡）、对抗（4 agent 最后生存者，毒圈 5 分钟收缩）、混合（2v2）、单人；每局上限 5 分钟。地图编辑器 + 几乎全部游戏参数可配 + 功能开关（Appendix D）。

### 1.3 时序模型（核心）
- **线程模型**（§3.2）：环境一个线程 + 每个 agent 一个线程，完全解耦，互不阻塞。形式化元组 (N,S,O,A,T,B_obs,B_act,ā,φ,Δt_fixed,r,T)。
- **观测缓冲**（Appendix H）：每个 agent 的 B_obs 存最新观测 + **观测版本号**；环境每生成一次联合观测版本号 +1；agent 以 Δt_poll 轮询，**只有版本号更新才开始新一轮推理**，否则短睡。
- **动作缓冲**：agent 推理完把动作写入 B_act；环境每个固定 tick Δt_fixed 读缓冲：**有有效动作就用一次，然后重置为无效；没有就执行默认动作 ā**。
- **默认动作 = [0,0,0,0,0,0,0]**（完全不动作，§4.1）。**没有显式的 per-decision deadline 机制**；隐式 deadline 就是下一个 tick——agent 慢于环境时，它缺席的每个 tick 都执行 no-op，动作迟到即失效（用一次就作废）。
- 度量：**effective action frequency** = 环境步中拿到新鲜有效动作（而非默认动作）的比例（Appendix L.1）——这正是「决策新鲜度」的现成量化。
- **tick 率**（§4.1）：turn-based + 两档实时 **Δt_fixed = 250 ms 与 40 ms**。40 ms ↔ 25 Hz 在 Appendix O 明确写出；250 ms ≈ 4 Hz（算术；注意 arxiv html 该句 Hz 标注渲染错乱为 "5 Hz"，以 250/40 ms 数值为��）。250 ms 对应人类简单反应时间，40 ms 对应机器人常用控制频率。
- **「暂停环境 vs 真实时」**：有——传统 **turn-based 接口**就是暂停模式（环境每步等所有 agent 推理完，§4.1）。**论文中没有 PDQ/LCRT 这两个词**（全文 grep 无命中）；turn-based vs MAAG 就是它的等价对照。

### 1.4 基线与关键数字
- 基线实现（论文描述，代码在仓库里待确认）：5 个 LMM 零样本直控（关 thinking，text+pixel，去中心化）：GPT-5.4 / Gemini-3.1 Pro / Claude Haiku-4.5（官方 API）+ Qwen-3.5 9B / Gemma-4 E4B（本地 H200）；**规则 FSM 启发式 bot + 随机策略**作校准基线（Appendix L.1）；**ReAct 式 agentic framework**（感知/记忆/上下文/规划/执行/反馈 6 模块，步内多轮推理，Appendix M）；另有 RL4VLM PPO+LoRA 微调 Gemma-4 E4B 的实验（Appendix O）。
- 端到端延迟 P50（Table 1）：GPT-5.4 **1458.5 ms**、Gemini-3.1 Pro **3235.2 ms**、Haiku 4.5 **1861.0 ms**、Qwen3.5-9B 96.6 ms、Gemma E4B 64.5 ms。
- 实时约束的破坏力（Table 1）：Gemini 合作分 **0.78（turn-based）→ 0.13（250 ms）→ 0.04（40 ms）**；GPT-5.4 0.31→0.07→0.02。
- 模态消融（Table 12，Gemini，250 ms）：**text-only 平均 0.12 vs pixel+text 0.18**；实时下加 depth/audio 无收益（延迟抵消）。turn-based 下 text-only 0.27 vs 全模态 0.90。
- 框架/集中式消融（Table 4）：agentic framework 在 turn-based 提升（Gemini 0.78→0.85），但 40 ms 下反而更差（0.04→0.03，延迟涨到 5880 ms）；Qwen-9B 因延迟低，框架在 250/40 ms 仍有小增益（0.00→0.05/0.04）。集中式控制的优势在 40 ms 下基本消失。
- 动机实验（Appendix F）：同为 4 agent，ROAR 的 Gemini 相对下降 93%（0.70→0.05），而 TeamCraft/TDW-Cook 在同样 250/40 ms 下基本稳定——**射击域对延迟远敏感于建造/烹饪域**。
- 每 agent 每步平均输入 **2122 tokens**；商业 API 每 agent 独立账号/key（RPM 5000/2000/4000）（Appendix L.1）。

### 1.5 本机可行性（Windows + CPU）
- 论文宣称（§1）：**单可执行文件分屏多人，无需多实例/网络连接；0.2 GB 磁盘+内存；可纯 CPU 渲染，无需独显**。实测环境侧开销（Ubuntu 24.04，Core Ultra 9 285K + RTX 5090）：渲染每步均值 5.6 ms；随机策略压测（Table 14）：game-state 观测 16 agent P90 27.8 ms、pixel 12 agent 38.5 ms、pixel+depth 8 agent 49.1 ms，内存 0.20–0.22 GB。
- **但作者全部实验只在 Ubuntu 24.04 上做**（Appendix L.1）；论文未提 Windows 支持、未给安装命令。Unity + ML-Agents 技术栈原则上跨平台（Python 侧 mlagents 包 + Unity 可执行文件），**Windows 可执行文件是否存在必须看仓库**——这是可行性结论的第一道闸。
- 要装什么（按论文推断，未验证）：ROAR 可执行文件 + Unity ML-Agents Toolkit 的 Python 环境；agent 侧就是我们自己的商业 API 客户端。不需要 GPU（宣称）。
- 商业 API 延迟（P50 1.5–3.2 s）≫ 250 ms tick → 大部分 tick 吃 no-op，effective action frequency 会很低、分数很低——**这恰是我们要研究的现象本身**，不是障碍；但别指望复现论文里本地小模型的分数。

---

## 2. Fresh at Decision, Stale at Action（arXiv:2610.18126）

来源：https://arxiv.org/abs/2610.18126 （v1 2026-10-13；Ayoub Merimi, Souhail Bakkali, Abdeljalil Sakat）；全文 https://ar5iv.labs.arxiv.org/html/2610.18126

### 2.1 域与任务
- **合成的个人助理决策流**（虚构实体，防止模型靠记忆答题；生成器与数据随论文发布，§5）。Direct 跨 5 个域（晚餐订位、铁路出行、销售/许可平台、山地徒步、购物等），Indirect 4 个域。
- 每个决策两个选项：选项 1 在决策时更优（评分/价格/行程/景观），依赖一个证据里点名的实体（街区、铁路运营商、平台、山脉、购物中心）；选项 2 可用但较差。**决策→执行间隔 3 小时–3 天；修订截止（revision deadline）在执行前 0.5–2 小时**。金标签完全由时间戳+陈述事实推出。

### 2.2 理论：六时间戳 + 新鲜度上限
- 六时间戳（§3）：t_w 证据写成、t_v 最后核验、t_d 决策、t_r 修订截止、t_a 执行、t_e 结果观测；t_w≤t_v≤t_d<t_r≤t_a≤t_e。
- 分解：t_a−t_v = Δ_fresh（t_d−t_v，检索/更新可压缩）+ Δ_act（t_a−t_d，任务定死）。
- **Freshness ceiling（Prop 1）**：事实以恒定风险率 λ_f 变化时，Pr[事实在 t_a 仍成立] = e^{−λ_f·Δ_fresh}·e^{−λ_f·Δ_act} ≤ **e^{−λ_f·Δ_act}**——决策时再新鲜也压不过执行间隙；剩余风险只能靠「执行间隙内发布、且在 t_r 前送达决策」的新信息消除。
- 准确率因子分解（Eq 3）：**A = (1−π)K + π[ρS + (1−ρ)L]**；π=应改决策占比，ρ=路由召回（决定性信息送达概率），S=送达后改的概率，L=无信息时保持的概率，K=应保持占比。**路由决定 ρ，判断决定 S/L**。

### 2.3 决策台账（ledger）记什么、怎么索引
- 台账是**跨会话持久状态**（§4）。决策时写入一条：选中项+备选项、t_a、t_r、选中项的证据、**dossier（背景事实包：依赖实体的别名、上游依赖，含决策时查到的大量无关材料）**。
- 每条新信息 x_j 发布时，router 给所有开放条目打分，送给 **top-K** 个决策；到 t_r 时模型按发布顺序读条目+送达信息，决定保持或更换。**没收到任何信息的决策不调用模型、直接保持**。
- 索引变体（§4/§6）：
  - **decision text**：BM25 或 dense（BGE-base）比对决策请求+证据 → 只能触达点名选项或其陈述依赖的新闻；
  - **whole dossier**：把全部背景事实并入索引 → 触达别名/上游，但也触达 dossier 里一切实体（暴露面大）；
  - **watch conditions**：**决策时由模型写 3–8 条「会发生什么就毁掉这次活动」的条件**，点名活动依赖的一切实体（含别名、上游），排除不需要的设施；BM25 作文档扩展，或句向量 max-cos 打分。这是「写入时做一次文档扩展」，由知道为什么选它的组件来写；
  - **time filter**：规则丢弃「开始时间晚于活动结束」的信息（把 P4 的时间检查用规则做）；
  - dependency queries（3 条固定查询）、Name match、None、All items（广播）、Oracle:related / Oracle:decisive（金标）。
- 决定性信息的四种触达路径：点名选项 → 点名陈述依赖 → 依赖的别名 → 上游实体；每往下一层，链接就从决策文本移进背景知识。

### 2.4 实验条件与关键数字（准确含义）
- 条件（§5）：Direct 110 决策（60 应改）；Stress 75（48 应改，加同名诱饵、名字扩展实体、点名选项的迟到/无害通知等硬负例）；Indirect 128（64 应改，其中 32 条决定性通知经别名/上游触达；每决策 12 条 dossier 事实含 6 个陷阱）。
- 模型（§6）：Direct 用 Qwen3-8B（本地贪心、关 thinking、自己写 watch conditions）；Stress 用 Claude Sonnet 4.6 / GPT-6.1 Sol / Qwen3-235B-A22B；Indirect 用 Claude Sonnet 4.6 / Qwen3-235B-A22B / Ministral 8B（附录另有 Mistral 7B）。温度 0、结构化输出；同条件内由 Claude 统一写 watch conditions，保证路由跨模型一致。
- **P1（上限成立）**：不路由时准确率恰好=应保持占比，所有模型相同：Direct 45.5% / Stress 36.0% / Indirect 50.0%。应改的决策全错——改变它的信息在决策之后才发布。
- **P2（送达即改，索引决定召回）**：决定性通知一旦送达，模型 **91–100%** 会改（引言；Indirect 节写 90.6–100%，跨所有 router/模型）→ 应改决策的准确率≈路由召回。**decision-text 索引对「别名」新闻召回 37.5%、对「上游实体」新闻召回 12.5%**（点名选项/陈述依赖=100%）。Indirect：Name match 召回 25%，BM25 decision 62%，+dossier 100%（但送 89 个陷阱），watch conditions 92%（20 个陷阱）。Claude Indirect 准确率：None 50.0 → BM25 78.9 → +dossier 93.0 → watch 91.4。Stress（新闻点名陈述依赖）：BM25 decision 送达 100% 决定性信息，三模型 72.0–73.3%（name）→ 85.3–98.7%。
- **P3（误送毁掉正确决策）**：送给「应保持」决策的每条信息都是误导。Claude 在任何 router 下保持 ≥87.5%；**Qwen3-235B 与 Ministral 8B 在所有 BM25/dense router 下保持率 <50%**。一个陷阱送达使无谓更换率上升：Claude 9.4→14.0%，Qwen 69.7→74.8%，Ministral 71.5→84.1%。广播 token 成本 = BM25 的 4.0×（Stress）/3.1×（Indirect）。**暴露面控制对判断力弱的模型收益最大**。
- **P4（剩余错误是时间判断）**：只给决定性信息（oracle）时所有模型 Stress+Indirect 全对；加入「相关但不决定性」新闻使 Indirect 三模型合计错 91 个决策、收益 0。弱模型会因「活动开始前已结束的变更」「活动开始后才生效的变更」放弃决策——**模型对「变更宣布」起反应，跳过「何时生效」检查**。用规则做 time filter：Qwen3-235B Stress 85.3→92.0%。
- 论文三设计原则（§8）：决策连同时间与依赖记下来；用决策时写好的索引把新闻路由到修订截止；复核每个变更的生效时间。

### 2.5 可借用设计（→ harness 原语）
- **决策台账**：每个有延迟执行的决策一条持久记录（六时间戳、选项+备选、证据、依赖 dossier）——直接对应我们的「决策台账+中断修订」。
- **watch conditions 作为订阅索引**：决策时写 3–8 条监视条件，事件流按条件匹配到开放决策（= harness 的 subscribe 原语该长的样子）；比全文检索暴露面小、比只匹配决策文本召回高。
- **revision deadline 一等公民**：到点触发复核；无��闻则零成本保持（不调模型）。
- **时间生效检查用规则不用模型**（time filter）：便宜且恰好补弱模型的短板。
- **top-K 路由预算 + 暴露面/召回权衡按模型判断力调**。

---

## 3. Agent Harness Awareness（arXiv:2611.05391）

来源：https://arxiv.org/abs/2611.05391 （v1 2026-09-13；Manu Agrawal, Shrey Nagpal，独立研究者；9 页）；全文 https://arxiv.org/html/2611.05391v1

### 3.1 验证什么遥测、怎么变成操作控制
- 缺口命名：**operational blindness**——harness 的监视器/trace/近期失败知道哪条路健康，选动作的模型不知道。
- **信任分离四步**（§2）：host 测量近期执行结果与约束 → **确定性 reducer** 选事实、设界、脱敏、附 provenance 与 freshness → 模型在语义合格的动作里用「任务+已验证快照」选择 → **host 复核当前状态、强制硬约束、派发或安全拒绝**。测量/强制/评估/最终派发权始终在 host；模型改不了遥测也绕不过强制。
- **SST（Substrate State Telemetry）契约**四维度：tools&network（本文已评估）；runtime（deadline、排队、并发、退出态——展望）；hardware/sandbox（内存余量、CPU 压力、存储——展望）；economics（上下文余量、工具调用预算、配额、价格——展望）。
- ToolRoute 每行路由遥测：探测窗口计数、成功数、**延迟 EWMA**、连续失败+最后错误、**熔断器状态**、**观测年龄**；快照头声明 freshness 界限。

### 3.2 observable policy regret（毫秒）定义
- r_obs(r,z) = c_obs(r,z) − min_{r'} c_obs(r',z)，即相对「同一可见事实下最优路由」的超额操作成本（Eq 1）。
- c_obs = ℓ + 10,000·p_f + 10,000·1[熔断 OPEN]（ms）；ℓ=延迟 EWMA，p_f=(n−s)/n（n=0 时取 0.5）（Eq 2）。**两个 10,000 ms 是冻结的工作流级失败/开路惩罚，不是线上实测延迟**。**0 ms regret = 零可避免操作成本，不是零请求时长**；完成但可避免地更慢/更险仍有正 regret。

### 3.3 216-decision cohort 与 A/B/C
- **Study 1**：216 = 3 模型族 × 6 seeds × 4 个预声明环境状态 × 3 条件；任务是经 tool_alpha/tool_beta 之一（或 wait）取一条只读客户记录；**不重试**，只测派发前首选。模型：GPT-5.6 Sol、Claude Sonnet 5、Gemini 3.7 Flash；模型无 shell/文件系统/网络工具/oracle 访问。
  - **A task-only**：任务+等价工具描述，无操作投影（部署基线）；
  - **B neutral envelope**：与 C **同形状**的遥测信封（字段、行序、工具行全同），两路由填相等中性值（结构对照）；
  - **C verified state**：同信封填 host 拥有的新鲜事实。
- 结果（§4.1）：B→C 平均 regret **4251.54→131.12 ms（−96.9%）**；单次派发完成 **50/72（69.4%）→69/72（95.8%），+26.4pp**。A：4826.45 ms、47/72（65.3%）；C vs A −97.3%、+30.6pp。C 中所有 69/69 次选活跃路由都命中可见最优、0 ms regret；剩 3 个 C 决策（每模型 1 个）在两路由都退化时选 wait，构成全部残余 regret。每模型 C 均 23/24，regret 75.50/75.50/242.35 ms。18 个模型×seed 块 bootstrap：Δregret 4120 ms [2543, 5893]，Δ完成 +26.4pp [18.1, 34.7]。
- **Study 2（活体复现，27 决策，永不与 Study 1 合并）**：3 模型 × 3 故障型 × A/B/C；本地 TCP/HTTP 代理，每路由 3 个监视请求，**OpenTelemetry client spans** → reducer（EWMA α=0.2，**2 s freshness 界限**；连续 3 次探测失败=OPEN，3 探窗口失败率 ≥0.4=DEGRADED）；**Toxiproxy** 注入预声明的 300 ms 延迟/连接失败/HTTP 503 之一；客户端超时 2 s；选中路由真实执行。B 5/9（8956.37 ms），**C 9/9（0 ms）**，A 6/9（6734.30 ms）。

### 3.4 可借用测量方法
- **同形状中性对照（B）**：把「信息值」与「接口形状值」分离——我们做 harness 原语 A/B 时照抄。
- **冻结预声明 manifest**：provider/model、条件、故障路由、选项顺序、执行顺序、no-retry 全部执行前定死。
- **可观测成本 oracle 只用 canonical 监视事实**，不用隐藏潜在异常；regret 以 ms 计。
- **artifact 归档**：prompt、脱敏请求/响应、快照、工具事件、评估结果、finalization hash 全留。
- Wilson CI + 模型×seed 块 bootstrap；两个研究永不合并。
- 每个快照带 freshness 界限；模型选完后 host 再复核（对应我们的「派发前验证」）。

---

## 4. Streaming Test-Time Compute（arXiv:2609.14995，可选）

来源：https://arxiv.org/abs/2609.14995 （v2 2026-09-15）；全文 https://arxiv.org/html/2609.14995v2 。立场论文（Xiaotian Zhang, Trooly.AI）：流式交互里证据增量到达且可被修订，早算 slack 多但证据不全、等则信息全但 slack 缩——**information–slack dilemma**。分析单元是「证据依赖的计算 job」（何时启动、什么支撑结果、何时可提交），分三个非互斥 regimes：**证据接地增量计算**（已有观测支撑：索引/计数/稳定主题检索）、**延续鲁棒准备**（对声明的可能延续家族都有用：通用事件索引、问题分解）、**未来依赖投机**（适用性取决于未证实假设，须保持条件态直到被支持/拒绝）。控制器动作空间 {wait, spawn(o,c,P), continue(j,c′), cancel(j), verify(j,c_v), commit(y)}，job 记录算子/启动时间/**证据版本**/依赖集/服务需求/产物；取消必须计入后端真正停止计费的时间。评估要分离早执行效应、相对全输入替代的部署价值、预测策略的附加价值，并计共享资源成本；恢复分显式依赖（局部失效，truth-maintenance 式）与隐式影响（须从检查点重建）。目标不是最大化提前计算，而是**声明资源包络内可信、准时的响应**。

---

## 5. 本机可行性与可借用设计

### 5.1 ROAR 可行性结论
- **有希望，但 Windows 未验证，仓库是第一道闸。** 支持面：0.2 GB 磁盘+内存、纯 CPU 渲染（论文宣称）、text/JSON+结构化 audio 的纯文本观测通道、7 维离散动作、环境侧开销极低（game-state 观测 16 agent P90<40 ms，桌面 CPU）——与「商业文本 LLM API + Windows + CPU」的约束形状吻合。
- 风险面：① 作者只在 Ubuntu 24.04 验证，Windows 可执行文件与安装步骤论文未给，**仓库 URL 与代码 license 本次未能确认**（论文只宣称 fully open-source）；② 商业 API P50 1.5–3.2 s ≫ 250 ms tick，多数 tick 将执行 no-op，分数会远低于论文——这是研究对象而非缺陷，但评估设计要按 effective action frequency 而非绝对分数来比；③ text-only 比 pixel+text 弱（0.12 vs 0.18 @250 ms），纯文本路线的天花板要心里有数。
- 下一步（不在本次侦察范围）：拿到仓库（先试 OpenReview 页面人工打开 / 作者主页 / 论文 PDF 脚注）→ 确认 Windows build 与 license → 临时 home 里起一局 turn-based 冒烟。

### 5.2 四个研究问题 → 现成设计映射
| 我们的问题 | 可直接借用的形状 | 出处 |
| --- | --- | --- |
| 观测 staleness | 观测版本号+时间戳（ROAR B_obs version index）；快照带 freshness 界限与观测年龄（SST） | ROAR Appendix H；AHA §2 |
| 决策台账+中断修订 | 六时间戳持久台账；watch conditions 作订阅索引；revision deadline 到点复核；无新闻零成本保持；时间生效检查用规则 | FaD §3–4 |
| deadline anytime 等待 | tick 缺席即默认动作+effective action frequency 度量；job 动作空间 wait/spawn/continue/cancel/verify/commit 带预算与证据版本；runtime SST（deadline/排队）是公开展望位 | ROAR §3.2/L.1；STTC §3.3；AHA §2 |
| 思考/行动调度 | 三 regimes（接地增量/鲁棒准备/投机）+information–slack；实时下框架与集中式的收益被延迟吃掉（延迟是系统级变量）；派发前注入已验证遥测 | STTC §3.1；ROAR Table 4；AHA §4 |
| 测量方法 | 同形状中性对照 A/B/C；冻结 manifest；ms 计 observable regret；精确标签合成流；Wilson+块 bootstrap；artifact 全归档 | AHA §3–4；FaD §5–6 |

### 5.3 诚实性声明
- 所有数字均为论文自述，本机���复现任何实验；未安装、未运行游戏。
- ROAR 仓库 URL、代码 license、Windows 支持：**未确认**，是后续工作的第一优先。
- ROAR 论文无 PDQ/LCRT 字样；其「暂停环境」模式即传统 turn-based Gym 接口。
- arxiv html 的 250 ms 档 Hz 标注渲染错乱（"5 Hz"），本报告按 250 ms≈4 Hz、40 ms=25 Hz（后者论文明写）记录。
