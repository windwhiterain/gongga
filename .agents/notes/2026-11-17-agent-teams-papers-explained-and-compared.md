# Agent teams：五篇论文核心点子详解 + 逐方面横向对比

Date: 2026-11-17. Session `agent_team`. 应用户要求：先详细解释每篇论文的核心点子，
再按「同一个方面、不同框架怎么实现」横向对比。对比对象限于有一手来源的系统：
Gongga（SPEC + 已装 skill）、DSH 0.2.2-rc.3（`39163dd30f`，新文档已读）、
以及五篇论文各自的系统。论文数字均为**自报**，未在本机复现。

来源：论文全文 `.dev/papers/*.txt`；DSH `docs/subsystems/{agent-team,subagent}.md`、
`packages/team/tool-agent-team/README.md`、`docs/tool-catalog.md`、重构 notes。

## 出处与操作笔记（合并存档）

本笔记由 dream 合并整理，**取代并退役**同日两份重叠笔记：
`2026-11-17-agent-teams-gongga-vs-dsh.md`（基于 DSH 0.2.1 的对比，其赌注表已被 0.2.2
重构修正）与 `2026-11-17-agent-teams-papers-read.md`（论文综述）。两者独有的耐久内容
已折入本节；论文详解与横向对比以本笔记为准。

**约束本计划的裁决（2026-11-15，见 `harness-exp-frontier-survey.md`）：** 已实现的特性
视为已验证，不做重复对比实验；实验只打 harness **没有的**特性，验证过的成为新特性。
垂直域转向见 `vertical-agent-research-directions.md`。

**DSH 更新出处：** `687f282e86`（0.2.1-rc.2，2026-10-17）→ `39163dd30f`（0.2.2-rc.3），
4824 个提交、11852 个文件，SSH 快进合并约 22 分钟。这一个月的 agent-team 变化是重构：
experimental 包删除（−7238 行），team 改为 subagent 树 + 四个任务工具 + 策略。来源 notes：
`2026-10-15-agent-teams-as-subagents`、`2026-10-16-tree-wide-agent-communication`、
`2026-10-19-team-task-board-inbox`、`2026-10-20-session-team-mode`、
`2026-10-29-inbox-only-agent-wait`、`2026-11-02-subagent-catalog-names`；升级指南
`docs/upgrade-guide/v0.2.1-rc.2/agent-team-subagents/guide.md`。

**arxiv 全文的可走网络路线（实测）：** flm 白名单模式（`arxiv.org` 在列）+ curl
`--ssl-no-revoke -x http://127.0.0.1:7897`。之前的 TLS 失败是本机 schannel 吊销检查离线
（CRYPT_E_REVOCATION_OFFLINE），不是代理节点；直连与 7890/10793 两个代理都失败，GitHub
SSH 通但慢。DeepSeek web-search 的 fetch 端点把正文截到约 8.5KB（摘要够、全文不够）。
论文全文本地副本在 `agent-teams` worktree 的 `.dev/papers/`：五篇的 `.html`/`.txt` +
转换器 `html2txt.py`。


---

# 第一部分：五篇论文的核心点子

## 1. Labtune（arXiv:2611.03935，Mitevski，2026-11-02）

**它要解决的问题。** 多 agent 科研系统里，「共享多少同伴信息」从来不是一个被控制的
变量：要么全共享（一个共写文件，谁都能读），要么不共享。作者认为这掩盖了一个核心权衡——
共享能避免重复踩坑，但也会让所有 agent 收敛到同一条路，杀死探索多样性。

**核心机制：把共享做成一个有刻度的政策 π=(level, wisdom)。** 五个 detail 级别
（Table 2，原文）：

| 级别 | 允许进入接收者上下文的内容 | 意图 |
|---|---|---|
| none | 无同伴研究内容，只有调度信息 | 保住独立搜索的基线 |
| low | **只有死路**，且每条必须挂一个有效的 development receipt；不给正面结果、排名、配方 | 不重复昂贵失败，但不引向解 |
| intermediate | 紧凑发现、宽泛方法描述、约束与权衡；**不给确切代码或调参配方** | 共享方向，限制抄袭 |
| high | 实验摘要、测量、方法、参数、允许的产物引用 | 刻意复用与重组 |
| wisdom_only | 只有条件化抽象（教训、适用条件、局限） | 给一般教训，少给解的引导 |

wisdom 是**独立开关**，所以有效配置是 8 个（五个命名模式 + low/intermediate/high 各自带
wisdom）。一条 wisdom 条目记录：证据提示的原则、适用条件、局限与例外、证据状态
（tentative / repeated within a scope / contradicted / superseded）、证据标识符。
接收者**只看到抽象**；首位身份、排名、确切参数、实现配方、建议的下一步都被扣下，
控制器保留完整归因。

**强制方式（这是它和「自由聊天」的根本区别）。** 团队只写各自的私有 outbox；在
**阶段屏障（phase barrier）**上，控制器对同伴输出施加过滤 V_π，为每个接收者发布一份
**不可变、带版本的 Markdown 视图**，并受字数读预算 W 截断：

  C_i(t+1) = C_i(t) ∪ trunc_W(V_π(o_{-i}(t)))

agent **从不共写一个研究文件**；事件存储才是权威，Markdown 只是精确的接收者视图，
带哈希留存以便重放。low 级别只放行证据引用能匹配有效 receipt 的死路；wisdom 只放行
lesson/conditions/limits 字段。会议也是调度任务而非聊天：先提交私有立场，再交一个限长
packet，每人一条观察/异议/带成本的提案，主持人记录共识 + 未决分歧 + 下一个检验。

**七条原则里最该抄的三条。** P3 委托守恒预算与权限（子 agent 消耗父的配额，不能拓宽父的
访问；拓扑不创造容量，等待的父让出容量）；P4 证据高于断言（评估器 receipt，模型自信不是
概率）；P5 受保护确认（候选事先固定、用不相交数据、结果永不回流给行动 agent）。

**关键事实：这是一篇想法/系统论文，没有任何实验结果。** 它给的是配置空间 + 参考实现 +
六个可预注册假设：H1 死路共享减少重复昂贵失败而不减少检验过的机制数；H2 wisdom 传递结构
同时保住多样性；H3 高细节下 wisdom 冗余；H4 高共享加速复用但收窄方法（净效应取决于问题
崎岖度）；**H5 同模型下组织差异随共享上升而缩小，异构模型反而放大**；H6 预算对齐时团队
只在可分子问题上胜过持久单 scientist，深度>1 的协调开销增长快于有用工作。协议是两阶段
析因（先固定共享变设计 A–F，再在选定设计内变 8 个共享配置），单 scientist 基线跑 1/3/6
遍且后几遍必须审计或攻击而非复述。**这个空间还没人测过。**

**局限与对我们的意味。** 作者自己点名两个威胁：同一模型上的 persona 可能只是名字不同——
要**测量真正执行的机制**；字段过滤保证不了语义隔离——**泄漏本身是要量化的结果**。
Gongga 的会话隔离是免费的 none；`send_message` 是自由聊天，恰是 Labtune 禁止当强制机制
用的东西，所以忠实臂需要一个控制器 skill（私有 outbox → 屏障 → 发布接收者视图）。
wisdom 条目的形状（教训/条件/局限/证据状态）和我们 dreaming 笔记、survey 的记忆库候选
同形。H5 直接关于 `model_pool` 异构。

## 2. IMACS / Org Science（arXiv:2607.25446，Chen et al.，2026-07-28）

**它要解决的问题。** 现有工作把「组织」和「协作算法」混在一起调，得出的「某组织更好」
其实可能是「某协议更好」。作者主张三层正交。

**核心机制：WHO × HOW × ALGORITHM 三层解耦。**
- WHO（组织）：角色、模型绑定、RACI 问责标签（每个交付物经典上恰好一个 Accountable）。
  三个可插拔预设：belbin（默认）、adhocracy、three-departments（用 Belbin 术语表达的
  唐代三省六部制）——证明组织层是「文化可插拔」的。
- HOW（协调）：Mintzberg 的五种协调机制（相互调整、直接监督、工作过程标准化、产出
  标准化、技能标准化）。
- ALGORITHM（协作协议）：六个已发表协议 behind 一个接口，
  A = {voting, moa, blender, debate, reflexion, plan-execute}。每个协议有相对模型调用
  倍率 c(π)：voting 1.0、moa/blender 1.5、reflexion 2.0、plan-execute 2.5、debate 3.0。
  建在 AgentScope 2.0 上。

**关键证据一：组织×协议是交互，不是主效应。** GSM8K（N=10/格）：belbin 在 voting/debate
下 0.60/0.80，adhocracy 0.70/0.70，three-departments 0.50/0.90。**一个固定协议换组织能差
0.20 exact-match**；没有哪个组织普遍最好。GSM8K 偏好 plan-execute 和 debate（0.90）胜过
voting（0.60）。

**关键证据二：噪声校准技巧（我们直接抄）。** voting/debate 根本不读 Accountable 标签，
所以 RACI 变体在这些协议下**功能上等价**——3 次重复均值差在 0.04 内，而单次跨变体散布达
**0.17±0.05**。这个散布就是 N=10 的噪声底：小于它的单次差异是生成噪声，不是效应。

**关键证据三：问责位置的机制发现（已复现）。** 问责人位置改变结果，**当且仅当协议把
交付物路由经过问责 agent**。LLM-Blender 的 fuser 是问责 agent：把 Accountable 从
Chair(glm-5.2) 移到 Critic(deepseek)，Critic−Chair 差在 3/3 重复为正（+0.08±0.02；
HotpotQA 0.34→0.48）；MoA（固定聚合器）是对照，洗牌。但获胜的 fuser 只在 3 个数据集中的
1、3、1 个上追平更强单模型——**方向不可先验预测，必须按模型绑定实测**。

**关键证据四：自适应组织路由。** LinUCB（d=7，α=0.6）按任务特征选协议，奖励 score−λ·cost。
模拟 oracle 研究 T=400、λ=0.3：router 0.708 vs 最佳固定（MoA）0.669，oracle 0.754——
收回 45% 的 headroom，最终路由准确率 82%，次线性 regret。去掉上下文特征的消融塌缩成
永远 voting（0.628）：增益来自上下文，不是 bandit 机器。真实奖励跑只是可行性（N=18/20，
策略集中到一个便宜臂）。

**对我们的意味。** polity 正是 IMACS 批评的「组织+协议纠缠 bundle」。`model_pool` 路由
模型，但没有任何东西路由协议。RACI 预测翻译过来：变 polity 最终交付物写在谁那里
（justice vs premier）× 模型档，预期只在交付物真流经问责会话时有效应。协议路由器是一个
有发表配方和效应量的 feature 候选。

## 3. OpenCollab（arXiv:2609.38345，Hsu et al.，v3 2026-10-18）

**它要解决的问题——「协作幻觉」。** 当模型自主决定时，委托很少真的发生：lead 绕过配置好的
队友独自解题，或在任何交接前耗尽预算。**声明的组织在默认情况下不到一半的 run 里被实现。**
prompt 强制委托会让 Adherence 剧烈摆动（措辞敏感）；把日志过滤到「协作了的 run」是
处理后选择偏差（agent 简单题独解、卡住才求助 → 子集偏难 → 反而惩罚团队臂）。

**核心机制一：六轴结构 Adherence（Table 17，原文精确定义）。** 每个 run 有声明侧
（配置）和实现侧（事件日志），每轴对比两者，裁决 ∈ {adherent, deviant, unverifiable}：

| 轴 | 实现值读自 | 偏离判据 |
|---|---|---|
| Participation 参与 | 花了 token 且产出≥1 个模型输出的队友 | 某声明队友没干活 |
| Delegation 委托 | 花了 token 且产出≥1 输出的队友 | 没有这样的队友（而该 regime 提供/要求委托） |
| Role boundary 角色边界 | 归因到每个 agent 的工具调用 | 执行了分配工具集之外的调用 |
| Budget sharing 预算共享 | 每个角色的 token 抽取 vs 自己的配额 | 某角色到了配额却没记录到拒绝 |
| Information flow 信息流 | 事件日志里的 message_agent 调用（每个臂） | 消息走了拓扑未声明的边 |
| Context policy 上下文策略 | 每轮施加的压缩级别 | 触发了非分配级别 |

run adherent **当且仅当每个适用轴都过**：adh(r) = ∏_x 1[v_x=adherent]。不适用的轴
（如单 agent 的委托）不参与合取，不算过。unverifiable **算作不依从**（缺证据只能压低、
不能抬高报告率）。在他们的运行时里，四轴由强制持有（角色边界：只提供分配工具、其余拒绝；
预算：超额拒绝并记录；信息流：未声明边拒绝；上下文：所有角色同策略），所以合取实际由
参与+委托决定，落到一个判据：**每个声明队友都花了 token 且产出≥1 模型输出**。

**核心机制二：CACE = ITT / Adherence，且必须算 A_r=0 对照。** ITT 是相对 Single 的
Pass@1 差；除以 Adherence 率得依从者平均因果效应。因果解释靠两个条件：单边依从
（Single 臂不可能实现目标 Team 组织）+ 均值排除（在 A_r=0 的任务上，分配 Team 而非 Single
不改变均值结果）。他们展示为什么必须验证后者：open-card 结果 ITT +5.6 → CACE +11.8，
但增益全在不依从任务上（+15.8）而依从任务是 −5.9——**+11.8 根本不是依从者效应**。
mandatory card：91.7% 依从，CACE +3.0 ≈ 配对估计。

**核心机制三：七个受控比较条件。** 五个运行时持有（基模型、每角色工具、token/成本预算、
压缩策略、通信拓扑）+ 两个验证（声明记录在实现轨迹旁边；两者互相评估）。审计十个系统——
AutoGen、AG2、LangGraph、SWE-agent、OpenHands、Claude Code、Codex CLI、**DeepSeek
Harness**、Inspect AI、HAL——**没有一个满足全部七条**。强制是调用前运行时闸，不是事后过滤。

**关键证据：约束抬高 Adherence，但 Adherence≠成功。** 参考团队 47.2%；read-only 工具
94.4%、no-edit 86.1%、强制 prompt card 91.7%、ring 拓扑 91.7%（vs star 47.2%）、4M 预算
77.8%。但 GPT-5.6-Luna 97.2% 依从却只有 61.1% Pass@1，而偏离的 Qwen3.8-Flash 47.2% 依从
却 75.0% Pass@1。轨迹机制：参考 adopter 给两个 coder 都发了 brief，然后垄断执行到预算
耗尽、从不提交队友工作；**让 adopter 只读就强制了委托**并成功采纳补丁。

**对我们的意味（这是改测量方法的一篇）。** Gongga 的全量 sqlite + 单一事件计数器 +
角色归因，正是他们造框架才拿到的基底——六轴我们算得起，还能满足没人满足的两条验证条件。
`explore` 模式就是 read-only 能力限制。暴露的缺口：**没有团队级 token 预算闸**（usage 按
会话测，不按团队强制）；每角色工具集只有粗粒度（work/explore）。声明=brief+会话配置；
实现=事件日志；一个日志审计 skill 是 feature 候选。每个实验都应同时报 Adherence 与成功，
CACE 只在算了 A_r=0 对照后才报。

## 4. AOrchestra（arXiv:2602.03786，Ruan et al.，v2 2026-02-07）

**它要解决的问题。** 子 agent 常被当成「另一个完整 agent」，继承父的全部上下文和工具，
既贵又噪。作者把 agent 拆成一个可合成的元组。

**核心机制：Φ=(I, C, T, M)。** I=指令，C=精选工作上下文，T=工具集（动作空间），
M=底层模型。明确分两轴：**工作记忆 (I,C)** 与 **能力 (T,M)**。编排器只有两个动作：
`Delegate(Φ_t)` 实例化一个执行器——它用 M_t 跑、**被限制在 T_t**、**只条件于 (I_t,C_t)**，
返回结构化观察 o_t（结果摘要、相关产物如文件/引用、失败时的错误/日志）；`Finish(y)` 终止
并输出最终答案。编排器**自己不做环境动作**。子 agent 是动态、即插即用的（ReAct 风格、
Mini-SWE 风格、Claude Code…）。

**关键证据一：上下文消融（GAIA，50 样本，其余持有）。** 精选上下文 **96.00** > 无上下文
86.00 > **全量上下文 84.00**。**全量继承平均比不给还差。** Gongga 原生有这三臂：brief 带
`@file` index（精选）、`clone_session`（全量）、裸指令（无）——是现成的已发表论断复现。

**关键证据二：主结果（Gemini-3-Flash 编排器）。** GAIA 80.00 pass@1（OpenHands 66.06）、
Terminal-Bench 2.0 52.86（Mini-SWE 34.29）、SWE-bench-Verified 82.00；平均比最强基线
+16.28% 相对（摘要；§4.2 写 22.13%——论文内部不一致，照实记）。

**关键证据三：编排可学习。** 弱编排器 Qwen3-8B 56.97 → SFT 68.48（复杂任务尝试 +56%）；
对编排器指令做 ICL 提示优化（带性能+成本反馈）：混模型 72.12@$0.70 → 75.15@$0.57
（更好且更便宜）。即插即用：ReAct 风格子 48.57 vs 独立 ReAct 28.57；Mini-SWE 风格 44.29
vs 34.29。学习分两维：任务编排（做什么、用什么上下文、用什么工具）+ 模型路由（选 M_t
平衡性能与成本）。

**对我们的意味。** `new_subagent` 已经合成 (I=brief, C=引用, M=model_pool)；**T 不能按子
会话配**——元组的能力轴缺一半。ICL 编排器循环就是 survey 的自我进化候选用在一条 prompt
上，有发表配方和效应量。

## 5. AnalogLayoutWorld（arXiv:2611.16991，NovaSilicon，2026-11-12）

**它要解决的问题。** 模拟版图设计能不能被 agent 做？更一般地：怎么给一个「产物存在≠做对」
的领域建可信基准。

**核心机制：任务 + 独立评分。** 40 个专家审核任务：固定 SKY130 晶体管净表 + 电气规格 +
可执行校验；所有电路通过布局前仿真（验证基线）。Docker + gdsfactory/KLayout(DRC,LVS)/
Magic(PEX)/ngspice；**没有 typed tools 或 MCP**——agent 用自己 harness 的原生执行工具；
网络隔离的评分容器；每次尝试 3 小时墙钟；提交契约（`submission/layout.gds` +
`submission/src/`）。**评分在终止后、全新目录、可信参照与评分代码独立引入；agent 自报
结果不决定分数。** 资格 E = I_GDS · I_LVS · I_GEO · I_M（提交 × LVS × 几何边界 × 对退化
版图的形态筛）；加权部分分（紧凑度、工程质量、电气 manifest 比例）；完全接受还要求
DRC + 所有电气 case + 完整证据。

**关键证据一：产物存在不是指标。** 13 个模型–harness 配置：**91.1% 的尝试产出 GDS，
只有 23.0% 满足接受**。

**关键证据二：迭代才是进展所在。** 90.3% 的 DRC 和 66.8% 的 LVS 首次通过发生在第一次
校验调用**之后**。

**关键证据三：难度描述符与预算行为。** 器件数是与 agent 无关的难度描述符（分数相关
ρ_s=−0.73）。GPT 系 agent 随难度缩放努力（工具调用 ρ_s≈0.66–0.68）并保留预算余量；
开源权重配置在小任务上饱和（GLM-5.3 Flash 在 51/120 次尝试撞到时间上限，GPT-6 Astra 为 0）。
效率：Astra 0.7483@11.2M token vs GLM-5.3 0.4779@34.5M。

**对我们的意味（任务套件设计范本）。** 独立评分器在团队预算之外；资格门 + 部分分
（不是产物存在的 pass/fail）；每任务一个与 agent 无关的难度描述符；预算余量与校验调用
次数作为过程指标；固定提交契约。它本身是单 agent 基准，没有团队——但它的评分纪律正是
我们团队实验缺的。

---

# 第二部分：逐方面横向对比

每方面给一张表（只列对该方面有立场的系统），再点出差异的本质与对我们实验的意味。
DSH 一律指 0.2.2-rc.3 已读机制；Gongga 指 SPEC + skill。

## A. 代理身份与团队边界

| 系统 | 身份是什么 | 团队怎么形成 | 边界强制 |
|---|---|---|---|
| Gongga | 会话路径（`main/impl/review`），层级命名 | 隐式：会话子树即团队；任何会话可在自己名下 spawn | 无显式团队域；名字撞了 `new_session` 拒绝 |
| DSH 0.2.2 | 统一 `SessionId`；模型面名字=根起 slash 串联（全命名时）否则 SessionId | **opt-in team mode**：根会话保存的选择，默认 off；团队=根+所有后代 | 只有根能 `setEnabled`；后代共享模式；历史 roster 不授权 |
| Labtune | 持久 Scientist 身份 + 角色/persona | 配置元组 L 里的 design D 定义 | 控制器持有身份与归因 |
| IMACS | 角色池 + RACI 标签 + 模型绑定 | 组织预设（belbin/adhocracy/three-departments） | 声明式、可校验的设计面 |
| AOrchestra | 动态 Φ 元组，无持久身份 | 编排器每次 Delegate 实例化 | 执行器只条件于 (I,C)、限制于 T |
| ALW | 单 agent/尝试 | 无团队 | — |

**本质。** DSH 0.2.2 放弃了 0.2.1 的扁平 roster + Lead-only-spawn，收敛到「Session 树 +
opt-in 团队模式」，结构上和 Gongga 会话树几乎同构。差别只剩：DSH 有显式开关与品牌 TeamId，
Gongga 是隐式子树。AOrchestra 走另一极端——agent 是一次性的合成元组，没有持久身份。
**实验意味：** 团队边界本身不是好变量（两家已收敛）；真正的变量是边界内的协调状态（B）
与共享（C）。

## B. 任务分解与协调状态（任务板）

| 系统 | 有没有持久任务结构 | 机制 |
|---|---|---|
| Gongga | **无** | 散文 brief + polity 的 JSONL 记录（skill 层，非 harness 域） |
| DSH 0.2.2 | **有，共享任务 DAG** | `TeamTaskSnapshot{id, revision, subject, description, status, ownerId, ownerName, blockedBy, writeScopes}`；revision 是 CAS（每次变更+1）；blockedBy 必须非删除且无环；status=pending/in_progress/completed/deleted(墓碑)；每个任务事件存完整快照 |
| Labtune | 调度任务（会议是任务） | 依赖就绪波（dependency-ready waves），屏障处发布 |
| IMACS | 协议定义工作流 | 六协议各自的融合过程 |
| OpenCollab | 声明的组织拓扑 | card/ring/star 等，运行时强制边 |
| AOrchestra | 编排器计划隐式在 Delegate 序列里 | 无持久板 |

DSH 四个任务工具：`team_task_create`（建无主 pending 任务：subject/description/blocked_by）、
`team_task_list`、`team_task_get`、`team_task_update`（task_id + expected_revision CAS +
action 转移）。**Team 不注册任何通信/列表/中断工具**——标准 subagent 工具服务所有人。
任务变更**不结束 inbox 等待、不启动 idle agent**；要 agent 对任务变更行动必须显式发消息。
`writeScopes` 是**规范化建议路径前缀，明确不是锁**（外部写者 bash/formatter/generator
绕过任何围栏，「假互斥比显式警告更危险」）；视图导出就绪性 + 写域重叠警告。

**本质。** 这是 DSH 一个月演化后**唯一幸存**的差异件——它把扁平 roster、Lead-only、
团队邮箱全删了，留下任务板。Gongga 完全没有持久协调状态。**实验意味：E1（任务板）被磨得
最尖**：最小 CAS/blockedBy/owner/建议写域 skill，板操作进事件流，协调本身变得可测。

## C. 信息共享（Labtune 的核心轴）

| 系统 | 共享机制 | 默认刻度 | 强制点 |
|---|---|---|---|
| Gongga | `send_message`（自由聊天，step 边界注入）+ 会话 transcript 隔离 + `clone_session` 全量 fork | 隔离=none；聊天=任意 | 无政策强制；隔离是会话边界副产品 |
| DSH 0.2.2 | 树内 inbox 消息（跨分支）+ 共享任务板 + 共享 checkout | 高共享（同 checkout、同板） | 无分级；消息任意内容 |
| Labtune | 控制器在屏障发布不可变接收者视图 V_π，字数预算 W | π=(level,wisdom)，8 配置 | **控制器强制**；agent 不共写文件；low 只放有 receipt 的死路 |
| IMACS | 协议决定谁看到什么 | voting 看答案、debate 看论证、MoA 看层 | 协议接口 |
| OpenCollab | message_agent 沿声明边 | 拓扑声明 | 未声明边拒绝（信息流轴） |
| AOrchestra | 编排器精选 C 传给执行器 | 精选（curated） | 执行器只条件于 (I,C) |
| ALW | 无共享 | 单 agent | — |

**本质。** 三种哲学：Gongga/DSH 把共享当**通信副产品**（自由消息）；Labtune 把共享当
**受控政策**（控制器过滤 + 屏障 + 读预算）；AOrchestra 把共享当**编排器的精选动作**。
Labtune 的论点正是：自由聊天不能当强制机制，因为一个被允许的链接不授予产物访问、协调者
不能向受限接收者转 relay 更富内容。**实验意味：E2 的忠实臂需要控制器 skill**（私有 outbox
→ 屏障 → 发布视图），不能只用 `send_message`。AOrchestra 的 curated/full/none 三臂 Gongga
原生都有，是现成复现。

## D. 委托权威与能力限制

| 系统 | 谁能 spawn | 谁能中断 | 能力限制 |
|---|---|---|---|
| Gongga | 任何会话在自己名下（`new_subagent`） | 命令层 `interrupt_session` 任意会话（无祖先限制） | 粗：mode work/explore（explore 拒写） |
| DSH 0.2.2 | 树内成员（subagent 控制） | **仅祖先**（`ancestor` 权威=目标创建 ancestry 里的活 Agent）或 `user`；Remote 需目标权限 | maxDepth 绝对深度帽；每后端 capability flags；agentPreset 独立组合 |
| Labtune | 委托限制 Δ=(深度,子数,活跃后代) | — | P3 子不能拓宽父访问；每角色工具授予 |
| IMACS | 组织图定义 | — | RACI 角色 |
| OpenCollab | 声明 regime | — | **每角色工具集运行时强制**（只读 adopter 94.4% 依从） |
| AOrchestra | 编排器 Delegate | — | **执行器限制于 T_t**（每子工具集） |

**本质。** Gongga 权威最平（任何会话可中断任何会话）；DSH 中断收紧到祖先。能力限制上
Gongga 最粗（只有 work/explore），AOrchestra/OpenCollab 最细（每子/每角色工具集）。
**实验意味：** OpenCollab 发现 read-only lead 强制委托——Gongga 的 explore 模式正是这个
限制，E4 加一个能力臂。Φ 的 T 轴（每子工具集）是 Gongga 明确缺口。

## E. 通知与等待

| 系统 | 怎么知道别人完成 | 等待原语 |
|---|---|---|
| Gongga | **事件订阅**：`subscribe(^turn_end:X$, send_message {event.answer})`，回执带整段答案 | `wait` 工具（等下一件到达，不消费）；**无轮询原语** |
| DSH 0.2.2 | subagent/end 事件（按树根 scope）+ 直接创建者在线时收最终文本/结构化输出 | `wait_agent`=**精确调用者 inbox 等待**；任务提交不结束等待；外部执行拒绝 follow-up |
| Labtune | 阶段屏障（同步） | 屏障 |
| IMACS | 协议轮次 | 协议定义 |
| OpenCollab | 拓扑边消息 | ring 传递 |
| AOrchestra | Delegate 同步返回 o_t | 编排器阻塞等执行器 |

**本质。** Gongga 是纯事件驱动（订阅 + 模板化答案，零轮询）；DSH 0.2.2 把 wait_agent 改成
普通 inbox 等待且**任务变更不唤醒**（要行动须显式发消息）；Labtune/AOrchestra 是同步屏障/
同步委托。**实验意味：** 通知机制不是好变量（都成熟）；但「任务变更是否唤醒」是 DSH 一个
明确设计选择，E1 任务板要决定板操作进不进事件流、唤不唤醒。

## F. 工作区与文件并发（E4 的对赌）

| 系统 | 默认 | 隔离机制 | 立场 |
|---|---|---|---|
| Gongga | **每会话一个 worktree** | 一写者/worktree（强制） | 隔离优先 |
| DSH 0.2.2 | **共享 checkout** | 0.2.2 新增**可选** worktree 工具：从 pinned commit 建分支+checkout、切换会话目录、离开保留分支 | 共享默认；隔离是显式调用者选择；自动 worktree 被明确拒绝 |
| Labtune | 私有工作区 + 控制器发布视图 | agent 不共写研究文件 | 私有 + 受控发布 |
| AOrchestra | 执行器作用于环境 | 上下文精选 | — |
| ALW | 每任务一个目录，评分在全新目录 | 评分隔离 | 评分独立 |

**本质。** 两个真实 harness 下了相反且都写进文档的赌注：Gongga 强制 worktree 隔离，
DSH 默认共享 + 建议写域（且 0.2.2 把 worktree 做成可选工具而非自动）。**没有人测过这个
对比。** 实验意味：E4 旗舰——跨文件依赖任务上 worktree 隔离 vs 共享 checkout + 建议写域，
两边基底都在本机。注意 DSH 现在有可选 worktree，所以 E4 是「默认赌注」对比，不是「有无
worktree」对比。

## G. 预算与强制

| 系统 | 预算单位 | 强制 |
|---|---|---|
| Gongga | 每会话 usage（context_tokens、usage 事件） | **只测不管**；无团队级闸 |
| DSH 0.2.2 | 成员帽 + maxDepth | 容量/深度强制；无团队 token 预算 |
| Labtune | 团队预算 B_p + lab 预算 | **P3 委托守恒**；字数读预算 W；每角色工具授予 |
| OpenCollab | 每角色 token 配额 | **超额拒绝并记录**（预算共享轴）；五条件运行时持有 |
| IMACS | 协议倍率 c(π) × 模型成本 | 路由奖励 score−λ·cost |
| AOrchestra | 每次 Delegate 成本 | ICL 带成本反馈 |
| ALW | 3h 墙钟/尝试 | 时间帽 |

**本质。** Labtune/OpenCollab 都把**预算对齐当前提条件**而非锦上添花；Gongga 只测不管。
**实验意味：** 析因开跑前要么先建团队预算闸，要么明确「只测」并把它当限制报告。这是用户
要拍板的第三个问题。

## H. 上下文继承

| 系统 | 机制 | 刻度 |
|---|---|---|
| Gongga | brief 首消息 + `@file`/`@skill` index（精选）；`clone_session` 逐行复制（全量）；裸指令（无） | **三臂原生** |
| DSH 0.2.2 | `context: fresh \| fork`；spawn 支持独立 agentPreset，omission/fork 继承父组合 | fresh/fork 两臂 |
| Labtune | 私有上下文 + 屏障接收者视图 | 政策控制 |
| AOrchestra | 编排器精选 C | curated/full/none（消融证明 curated 最好） |

**本质。** AOrchestra 已证明全量继承平均比不给还差（84 vs 86 vs 96）。Gongga 三臂都有，
DSH 两臂。**实验意味：** E2 折进 AOrchestra 上下文消融，是零代码的已发表论断复现。

## I. 异构与模型路由

| 系统 | 异构能力 | 路由 |
|---|---|---|
| Gongga | 同 harness，`model_pool` 按会话抽档（flash/pro/sota） | 模型路由有；**协议路由无** |
| DSH 0.2.2 | **后端异构**：spawn/fork（本地多轮）+ codex/claude-code/acp/dsh-sdk（单执行 handle，无多轮/inbox） | 每子选 provider/preset |
| Labtune | M=每角色模型分配 | H5 假设异构放大组织差异 |
| IMACS | 每角色模型绑定 | **LinUCB 协议路由**（0.708 vs 0.669） |
| AOrchestra | 即插即用跨框架子 agent | 模型路由 + 编排可学习（SFT/ICL） |
| OpenCollab | 基模型 held（受控条件） | — |

**本质。** DSH 异构在**后端**（能驱动 claude-code/codex），Gongga 异构在**模型**（同
harness 不同档）。IMACS/AOrchestra 证明编排/协议本身可学习、可路由。**实验意味：** E5
异构团队（Gongga lead + DSH 后端 workers）本机基底独特；协议路由器是 feature 候选。

## J. 评估与 grading

| 系统 | 评分纪律 |
|---|---|
| Gongga | polity justice 环外裁判；`.dev/verify` 数据；实验需自建独立评分器 |
| DSH 0.2.2 | Lead 集成边界（最终 diff + 测试是 Lead 的） |
| Labtune | P4 评估器 receipt；P5 受保护确认（候选固定、数据不相交、结果不回流） |
| IMACS | 噪声校准重复（功能等价变体）；≥3 重复 + 配对效应量 |
| OpenCollab | 配对 ITT/CACE + 六轴 Adherence；A_r=0 对照必算 |
| AOrchestra | benchmark pass@1 |
| ALW | **终止后独立评分**；资格门 + 部分分；agent 自报不算分；难度描述符 |

**本质。** 把五篇的评估纪律叠起来就是我们实验的测量栈：ALW 的独立评分 + 资格门，
OpenCollab 的 Adherence/CACE，IMACS 的噪声校准重复，Labtune 的受保护确认。Gongga 的
事件基底让这套算得起。**实验意味：** 套件先行，按这套纪律建；每个 run 声明与实现并排记录。

---

# 收敛：实验计划（与上一轮一致，此处给出对比支撑）

横切：每 run 记声明（brief+config）与实现（事件轨迹），报六轴 Adherence；≥3 重复 +
功能等价变体校准噪声；预算对齐、委托守恒（暴露团队预算闸缺口）。

- 套件先行（ALW 纪律 + OpenCollab 七条件清单）。
- E2 共享政策（Labtune 8 配置析因 + AOrchestra curated/full/none 消融，三臂原生）。
- E3 折进 E2 格子（IMACS WHO/HOW/ALGORITHM 逐层变 + RACI 位置×模型绑定）。
- E1 任务板（DSH 唯一幸存差异件；CAS/blockedBy/owner/建议写域 skill）。
- E4 隔离对赌（Gongga worktree vs DSH 共享 checkout，都是默认赌注）+ read-only lead 能力臂。
- E5 异构团队（Gongga 模型异构 + DSH 后端异构）。

新暴露 feature 候选：每子工具集（Φ 的 T）、团队预算闸、协议/组织路由器、声明-实现审计 skill。

待用户拍板：(1) 顺序；(2) 套件域（本仓编码 vs 复用 ALW 开源 EDA 任务）；(3) 预算闸先测还是先建。