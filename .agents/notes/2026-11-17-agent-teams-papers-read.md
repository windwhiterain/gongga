# Agent-teams papers read: what they change in the experiment plan

Date: 2026-11-17. Session `agent_team`. Follows
`2026-11-17-agent-teams-gongga-vs-dsh.md` (the DSH comparison). User instruction:
"看论文" — read the papers behind the proposals before choosing experiments.

Read in full (arXiv HTML, local copies under `.dev/papers/*.txt` in the
`agent-teams` worktree): Labtune 2611.03935v1, IMACS 2607.25446v1, AOrchestra
2602.03786v2, OpenCollab 2609.38345v3, AnalogLayoutWorld 2611.16991v1.
Network route that worked: flm whitelist mode + `arxiv.org` entry + curl
`--ssl-no-revoke` via 127.0.0.1:7897 (the local schannel revocation check was
the TLS failure, not the proxy node; direct and both other proxies fail).
All numbers below are as reported by the papers, not reproduced here.

## 1. Labtune (2611.03935, Mitevski, 2026-11-02) — E2's basis, upgraded

An **idea/system paper, explicitly not a benchmark result**: a configuration
space + executable reference implementation + six pre-registerable hypotheses.
Nobody has measured this space yet; a faithful run here would be among the first.

- Configuration tuple `L = (D, P, Δ, π, M, B, σ)`: design, persona assignment,
  delegation limits (depth, children, active descendants), sharing policy
  `π = (level, wisdom)`, models, budgets, statistical family. Hashed identity
  stored with every run.
- **Sharing levels** (Table 2): `none` (scheduling only) / `low` (**dead ends
  only, each backed by a valid development receipt** — no positive results) /
  `intermediate` (compact findings, methods, constraints — no exact code or
  recipes) / `high` (summaries, measurements, parameters, artifact refs) /
  `wisdom_only` (conditional abstractions: lesson, conditions, limits, evidence
  status ∈ tentative/repeated/contradicted/superseded). Wisdom is an independent
  flag → 8 useful configurations.
- **Enforcement**: controller publishes immutable, versioned, recipient-specific
  Markdown views **at phase barriers**, under a word read-budget; agents never
  write a common research file; the event store stays authoritative; views are
  retained with hashes for replay. A permitted link does not grant artifact
  access; a coordinator cannot relay richer content to a restricted recipient.
- Seven principles worth copying: P2 sharing is policy, not side effect;
  **P3 delegation conserves budget and permissions** (team budget `B_p`, lab
  budget; topology creates no capacity; a waiting parent yields capacity);
  P4 evidence outranks assertion (evaluator receipts; model confidence is not a
  probability); **P5 protected confirmation** (candidate fixed beforehand,
  disjoint data, results never return to acting agents); P6 exposure history is
  part of the result (mixed-exposure runs labelled); P7 no hidden self-redesign.
- Meetings are scheduled tasks, not chat: commit a private position first, then
  a size-limited packet, one observation/objection/costed proposal each,
  facilitator records agreement + unresolved disagreement + next test;
  consensus without evidence stays unsupported; minority views kept.
- **Hypotheses** (stated as possibilities): H1 dead-end sharing reduces repeated
  costly failures without reducing distinct mechanisms tested; H2 wisdom
  transfers structure while preserving diversity; H3 wisdom redundant at high
  detail; H4 high sharing speeds reuse but narrows approaches (net effect
  depends on problem ruggedness); **H5 same-model homogeneity shrinks org
  differences as sharing rises; heterogeneous models widen them**; H6 at matched
  budget teams beat a persistent single scientist only on separable sub-problems,
  and depth > 1 adds coordination overhead faster than useful work.
- Protocol: two-stage factorial (stage 1 design A–F at fixed sharing; stage 2
  the 8 sharing configs within selected designs), matched model/budget/tasks,
  single-scientist baseline (1/3/6 passes, later passes must audit or attack,
  not restate), fresh-context replicates. Outcomes: verified improvement on
  protected confirmation, total realized cost incl. meetings/retries/failures,
  duplicate experiments, communication overhead, range of mechanisms actually
  tested, approach-abandonment after reading a peer view.
- Threats they name: personas on one model may differ in name only — **measure
  the mechanisms executed**; field filters cannot guarantee semantic isolation —
  **leakage is an outcome to quantify**.

Gongga mapping: session isolation is a free `none`; `send_message` is free chat,
which Labtune forbids as an enforcement mechanism — the faithful arm needs a
controller skill (private outbox files → barrier → published recipient views).
The wisdom entry shape (lesson/conditions/limits/evidence status) is the same
shape as the survey's memory-store candidate and our dreaming notes. H5 is
directly about `model_pool` heterogeneity.

## 2. IMACS / Org Science (2607.25446, Chen et al., 2026-07-28) — E3's basis

- **Three orthogonal layers**: WHO (organization: roles, models, RACI
  accountability) × HOW (coordination: Mintzberg's five) × ALGORITHM
  (collaboration protocol: six published, behind one interface). Our E3
  "topology" variable conflated all three; they must be varied one at a time.
- Org × protocol interaction is real (GSM8K, N=10/cell): belbin 0.60/0.80
  (voting/debate), adhocracy 0.70/0.70, three-departments 0.50/0.90; a fixed
  protocol moves up to **0.20 exact-match** across organizations; no org is
  uniformly best.
- **Noise calibration trick**: voting/debate never read the Accountable tag, so
  RACI variants are functionally identical there — means agree within 0.04 over
  3 repeats while single-run cross-variant spread reaches **0.17 ± 0.05**. That
  spread is the N=10 noise floor: single-run differences of that size are
  generation noise. Our cells need ≥3 repeats and paired effect sizes.
- **Mechanistic finding (replicated)**: accountability placement changes
  outcomes **exactly when the protocol routes the deliverable through the
  accountable agent**. LLM-Blender's fuser is the accountable agent: moving
  Accountable Chair(glm-5.2)→Critic(deepseek) gives a Critic−Chair gap positive
  in 3/3 repeats (+0.08 ± 0.02; HotpotQA 0.34→0.48); MoA (fixed aggregator) is
  the control and shuffles. But the winning fuser matched the stronger solo
  model in only 1, 3, 1 of 3 datasets — **direction is not predictable a
  priori; placement must be found empirically per model binding**.
- **Adaptive Org Routing**: LinUCB (d=7, α=0.6) over task features, reward
  `score − λ·cost`. Simulated oracle study T=400, λ=0.3: router 0.708 vs best
  fixed (MoA) 0.669, oracle 0.754 — 45% of headroom recovered, 82% final routing
  accuracy, sublinear regret. Context-free ablation collapses to always-voting
  (0.628): the gain comes from context, not bandit machinery. Real-reward runs
  are feasibility only (N=18, N=20; policy concentrates on one cheap arm).

Gongga mapping: polity is exactly the entangled org+protocol bundle IMACS
criticizes. `model_pool` routes models; nothing routes protocols. The RACI
prediction translates: vary where polity's final deliverable is written
(justice vs premier) × model tier, and expect an effect only when the deliverable
actually routes through the accountable session. A protocol router is a
candidate feature with a published recipe and effect size.

## 3. OpenCollab (2609.38345, Hsu et al., v3 2026-10-18) — methodology for ALL runs

The most important paper for how we measure, not what we vary.

- **The illusion of collaboration**: when models decide autonomously, delegation
  rarely happens — the lead bypasses configured teammates and solves alone, or
  exhausts its budget before any handoff. Declared organization is realized
  **below half the time** by default. Prompt-mandating delegation swings
  Adherence wildly (phrasing sensitivity); filtering logs to collaborating runs
  is post-treatment selection bias (agents solve easy tasks alone, ask help when
  stuck → the subset skews harder → penalizes the team arm).
- **Adherence**: six audit axes — participation, delegation, role boundary,
  budget sharing, information flow, context policy. Per-axis verdict ∈
  {adherent, deviant, unverifiable}; a run is adherent iff **every applicable
  axis passes**; rate = fraction of adherent runs. Computed from structured
  event streams with role-level attribution, not prose logs.
- **CACE = ITT / Adherence** (Pass@1 difference from Single, rescaled). Requires
  mean exclusion; they compute the `A_r=0` Team-vs-Single contrast instead of
  assuming it. Their open-card result shows why: ITT +5.6 → CACE +11.8, but the
  gain sits entirely on non-adherent tasks (+15.8) while adherent tasks show
  −5.9 — **+11.8 is not a complier effect**. Mandatory card: 91.7% adherent,
  CACE +3.0 ≈ paired estimate.
- **Seven controlled-comparison conditions**: five held at runtime (base model,
  per-role tools, token/cost budgets, compaction policy, communication topology)
  + two verified (declaration recorded next to realized trajectory; the two
  evaluated against each other). Audit of ten artifacts — AutoGen, AG2,
  LangGraph, SWE-agent, OpenHands, Claude Code, Codex CLI, **DeepSeek Harness**,
  Inspect AI, HAL — **none meets all seven**. Enforcement is pre-call runtime
  gates, not post-hoc filtering.
- **Constraints raise Adherence** (reference team 47.2%): read-only tools 94.4%,
  no-edit 86.1%, mandatory prompt card 91.7%, ring topology 91.7% (vs star
  47.2%), 4M budget 77.8%. But **Adherence ≠ success**: GPT-5.6-Luna is 97.2%
  adherent at 61.1% Pass@1 while deviant Qwen3.8-Flash is 47.2% at 75.0%.
- Trace mechanism: the reference adopter briefs both coders, then monopolizes
  execution to budget exhaustion and never submits teammate work; making the
  adopter **read-only forces delegation** and successful patch adoption.

Gongga mapping: everything-in-sqlite + one event counter + role attribution is
exactly the substrate they built a framework to get — we can compute all six
axes cheaply, and meet the two verification conditions no audited artifact
meets. `explore` mode is the read-only capability restriction. Gaps exposed:
**no per-team token-budget gate** (usage is measured per session, not enforced
per team), per-role tool sets only coarse (work/explore). Declaration = brief +
session config; realization = event log; an audit skill over the log is a
candidate feature. Every experiment we run should report Adherence alongside
success, and CACE only with the `A_r=0` contrast computed.

## 4. AOrchestra (2602.03786, Ruan et al., v2 2026-02-07) — generated specialists

- Agent = tuple **Φ = (Instruction, Context, Tools, Model)**; working memory
  (I,C) vs capabilities (T,M). Orchestrator only `Delegate(Φ)` / `Finish(y)` —
  never environment actions. Sub-agents are dynamic, plug-and-play across
  frameworks (ReAct-style, Mini-SWE-style, Claude Code…).
- **Context ablation (GAIA, 50 samples, everything else held)**: curated
  context **96.00** > no context 86.00 > **full context 84.00**. Full inheritance
  is worse than none on average. Gongga natively has all three arms: brief with
  `@file` index (curated), `clone_session` (full), bare instruction (none) — a
  ready-made replication of a published claim on our substrate.
- Results (Gemini-3-Flash orchestrator): GAIA 80.00 pass@1 (OpenHands 66.06),
  Terminal-Bench 2.0 52.86 (Mini-SWE 34.29), SWE-bench-Verified 82.00; average
  +16.28% relative over strongest baselines (abstract; §4.2 says 22.13% — as
  reported, inconsistent within the paper).
- **Orchestration is learnable**: weak orchestrator Qwen3-8B 56.97 → SFT 68.48
  (+56% attempts on complex tasks); ICL prompt optimization of the orchestrator
  instruction with perf+cost feedback: mixed-model 72.12 @$0.70 → 75.15 @$0.57
  (better and cheaper). Plug-and-play: ReAct-style sub 48.57 vs standalone
  ReAct 28.57; Mini-SWE-style 44.29 vs 34.29.

Gongga mapping: `new_subagent` already synthesizes (I=brief, C=references,
M=model_pool); **T is not per-child configurable** — the tuple's capability axis
is half missing. The ICL orchestrator loop is the survey's self-evolution
candidate applied to one prompt, with a published recipe and effect size.

## 5. AnalogLayoutWorld (2611.16991, NovaSilicon, 2026-11-12) — task-suite design

- 40 expert-reviewed tasks: fixed SKY130 transistor netlist + electrical specs +
  executable validation; all circuits pass pre-layout simulation (verified
  baseline). Docker + gdsfactory/KLayout(DRC,LVS)/Magic(PEX)/ngspice; **no typed
  tools or MCP** — agents use their harness's native execution tools;
  network-isolated grading containers; 3 h wall-clock per attempt; submission
  contract (`submission/layout.gds` + `submission/src/`).
- **Grading**: after termination, in a fresh directory, trusted references and
  grading code introduced separately; **agent-reported results do not determine
  the score**. Eligibility `E = I_GDS · I_LVS · I_GEO · I_M` (submission × LVS ×
  geometry bounds × morphology screen against degenerate layouts); weighted
  partial credit (compactness, engineering quality, electrical manifest
  fraction); complete acceptance additionally requires DRC + all electrical
  cases + complete evidence.
- Headline: **91.1% of attempts produce a GDS, only 23.0% satisfy acceptance**
  (13 model–harness configurations). Artifact existence is not the metric.
- Process findings: 90.3% of DRC and 66.8% of LVS first passes happen **after
  the first verification call** — iteration is where progress lives. Device
  count is an agent-independent difficulty descriptor (score correlation
  ρ_s = −0.73). GPT-series agents scale effort with difficulty (tool calls
  ρ_s ≈ 0.66–0.68) and keep budget headroom; open-weight configs saturate on
  small tasks (GLM-5.3 Flash hits the time limit in 51/120 attempts vs 0 for
  GPT-6 Astra). Efficiency: Astra 0.7483 @ 11.2M tokens vs GLM-5.3 0.4779 @
  34.5M.

Suite requirements this fixes for us: independent grader outside the team's
budget; eligibility gates + partial credit (not pass/fail on artifact existence);
an agent-independent difficulty descriptor per task; budget-headroom and
verification-call counts as process metrics; fixed submission contract.

## 6. DSH updated to 0.2.2-rc.3 — the comparison corrected

Fetch landed: `687f282e86 → 39163dd30f` (4824 commits, 11852 files, ff-merge;
SSH ~22 min). The month's agent-team change is a **restructure, not refinements**:

- `packages/experimental/agent-team` and `tool-agent-team` deleted (−7238
  lines); team now lives at `packages/team/tool-agent-team` and **supplies only
  the four task tools + policy**. Notes: `2026-10-15-agent-teams-as-subagents`,
  `2026-10-16-tree-wide-agent-communication`, `2026-10-19-team-task-board-inbox`,
  `2026-10-20-session-team-mode`, `2026-10-29-inbox-only-agent-wait`,
  `2026-11-02-subagent-catalog-names`; upgrade guide
  `docs/upgrade-guide/v0.2.1-rc.2/agent-team-subagents/guide.md`.
- One identity: the Session id. `spawn_teammate` and team-scoped
  send/list/interrupt removed; standard subagent controls address by `agent_id`;
  **any member of one root tree can message any other across branches**;
  interruption stays ancestor-only. `list_agents` lists the whole tree.
- Team = root + all descendants; descendants at any depth share the board and
  can own tasks (`ownerId`; root reassigns to any reachable continuable
  descendant). Team mode is a **saved per-root-session selection, default off**;
  disabling keeps journal and children.
- `wait_agent` is now an ordinary subagent control waiting on the caller's
  inbox; **task commits do not end waits or start idle turns** — message an
  agent to make it act on a task change. `noProgress` removed.
- **The task-board and shared-checkout decisions remain active.** Separate
  working directories stay explicit caller choices.

Consequence for the bet table: DSH abandoned the flat-roster/Lead-only-spawn
bet and converged on a tree with tree-wide messaging — structurally close to
Gongga's session tree. What survives as genuinely different: the **durable task
DAG** (Gongga has none), opt-in team mode, inbox-wait vs event-subscribe,
shared-checkout default, heterogeneous backends, structured output capture.
DSH's own evolution discarded everything except the board — strong evidence
the board is the load-bearing piece, which sharpens E1.

## 7. Revised experiment plan

Cross-cutting (new, from OpenCollab): every run records its declaration
(brief + config) beside its realized event trajectory; report **Adherence on
the six axes** alongside success; paired task-by-task contrasts vs a single
agent; CACE only with the `A_r=0` contrast computed; ≥3 repeats per cell with a
functionally-identical variant as noise calibration (IMACS's trick); matched
budgets with delegation conserving budget (Labtune P3) — which exposes our
missing team-budget gate: measure first (usage events exist), enforce later.

- **Suite first** (unchanged, now specified): small multi-part tasks with
  cross-part dependencies; ALW-style independent grading outside the team
  budget, eligibility gates + partial credit, difficulty descriptor, submission
  contract; OpenCollab's seven conditions as the checklist.
- **E2 sharing policy** → Labtune-faithful: the 8 (level × wisdom) configs +
  none, controller-published recipient views at phase barriers (a skill:
  private outboxes → barrier → immutable views), protected confirmation,
  H1–H6 pre-registered; leakage and mechanisms-executed as outcomes. Fold in
  AOrchestra's context ablation (curated brief / full clone / bare instruction)
  — all three arms exist natively in Gongga.
- **E3 organization** → IMACS-faithful: decouple WHO/HOW/ALGORITHM; vary one at
  a time; RACI-placement × model-binding with the mechanistic prediction
  (effect only when the deliverable routes through the accountable session);
  optional LinUCB protocol router as the feature candidate.
- **E1 task board** → unchanged and reinforced: minimal CAS/blockedBy/owner/
  advisory-write-scope skill; DSH kept exactly this piece. Board operations join
  the event stream, so coordination itself becomes measurable.
- **E4 isolation bet** → alive (DSH re-confirmed shared checkout); add the
  capability-restriction arm OpenCollab found decisive (read-only lead forces
  delegation; Gongga's explore mode is that restriction).
- **E5 heterogeneous team** → later; AOrchestra's plug-and-play numbers give
  the expected shape; DSH backends (claude-code/codex/acp) exist on this machine.

Newly visible feature gaps (candidates, need the user's pick): per-child tool
sets (the T of Φ), team-budget enforcement, protocol/org routing, and a
declaration-vs-realization audit skill over the event log.

## 8. Open questions (sharpened)

1. Order: the recommendation is now suite → E2 (Labtune factorial + AOrchestra
   context ablation) → E1 (board) → E4 (isolation + capability arms) → E5, with
   E3's IMACS factors folded into E2's cells rather than a separate run. Accept?
2. Suite domain: this repo's coding tasks, or the open EDA vertical
   (AnalogLayoutWorld is open, SKY130 toolchain, independent grading already
   built — the suite could reuse its tasks directly)?
3. Budget gate: measure-only first (usage events), or build enforcement before
   the factorial runs? Labtune/OpenCollab both treat matched budgets as a
   precondition, not a nicety.
