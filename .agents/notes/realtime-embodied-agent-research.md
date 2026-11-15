# Real-time / embodied agent research (discussion, 2026-11-15)

User's question: real-time domain agent research/benchmarks — physical AI, embodied
intelligence. Follow-up to vertical-agent-research-directions.md. All claims as
reported by sources (web-search), untrusted.

## The core research problem

The latency gap: LLM deliberation is seconds-to-minutes; physical control is
ms-to-Hz. REA's measurements: ASPIRE's repair loop spent a median of 43 min per
LIBERO task; a Codex+GPT-6 pipeline consumed 89M input tokens for 60 LIBERO-Pro
tasks; models emitting one-token/IK actions directly solved ~1 of N. Consensus
architecture: asynchronous hierarchy — slow deliberative brain + fast reactive
policy, "no running policy ever waits on the brain".

## Benchmarks: embodied exams for coding agents (the bridge)

- EFE, Embodied First Exam (arXiv:2611.12177, 2026-11; Li Manling, Ruohan Zhang,
  Guanya Shi + Fei-Fei Li, Dawn Song, Abbeel): 1156 tasks, 35 suites, 47 robots;
  agent-native; asks what general coding agents can solve under physical
  constraints and where failures happen. 26 adapted suites + 9 new (spatial
  puzzles, dexterous manipulation, multi-robot, dynamic whole-body control,
  cross-simulator engineering). The headline "coding agent's first embodied exam".
- RobotAgentBench (2026-10, Aether AI/UCSD/UC Davis): Isaac Sim, 50 home layouts;
  coding agents vs standalone VLA/world-action-model policies under one
  plan-execute-verify framework; two-layer scoring (task abilities + open
  scenarios). Best agent 48.5 task / 15.4 scenario vs best standalone policy 18.0.
- CaP-Gym / CaP-Bench (Berkeley 2026): Code-as-Policy gym exposing the FULL
  primitive stack down to joint-level perception/control; abstraction tiers;
  studies agentic inference-time strategies; BEHAVIOR mobile manipulation.
- COIN (arXiv:2604.16886): quantifies the reasoning-vs-execution gap; visual
  understanding != motor execution; LIBERO saturation != capability.
- RoboCasa365 (2026) + failure-diagnosis-to-agentic-intervention (arXiv:2610.19600).

## Real-time architectures

- REA, Routed Embodied Agent runtime (arXiv:2610.42053, NUS/Berkeley/Tokyo):
  three-level brain-cerebellum split, asynchronous routing; LIBERO-Pro 95.9%,
  control steps -25%, control time -17%.
- AFSP, Agentic Fast-Slow Planning (arXiv:2604.01681, ICRA 2026): LLM reasoning
  bridged to real-time driving control; smoother, dynamically feasible trajectories.
- SFCo-Nav (arXiv:2603.01477): slow LLM + fast attributed-graph alignment;
  asynchronous LLM triggering by INTERNAL CONFIDENCE; legged-robot deployment.
- Token-wise latent streaming (arXiv:2607.16806): slow reasoner hidden states
  streamed into a fast planner via a latent bridge (dynamic VLN).

## Real-time benchmarks (accessible, non-physical)

- ROAR (arXiv:2610.24282, 2026-10): multiplayer action-shooter under real-time
  constraints; Multi-Agent Asynchronous Gym — observation/action buffers, the
  environment advances at fixed ticks while agents reason at their own pace;
  0.2 GB, CPU rendering, no GPU; multimodal obs (pixels/depth/text/state/audio).
- OmniGameArena (arXiv:2606.09826): UE5 VLM game agents; two clock modes — PDQ
  (pause env during inference: pure decision quality) vs LCRT (latency-controlled
  real-time). Methodology worth copying for any real-time harness evaluation.
- Win Fast or Lose Slow (NeurIPS 2025): speed-accuracy tradeoffs under strict
  latency (HFT, competitive gaming).
- DOOM small-model study (arXiv:2604.07385): small specialized models beat
  general ones; frame_skip as the latency control.

## Gongga fit

The async brain-cerebellum pattern maps onto existing machinery:
- channels = the fast loop as a detached process; observation/action buffers are
  channel I/O; the brain reads/intervenes via exec input / wait / until.
- wait/interrupt semantics = anytime interruption; send_message already
  interrupts a waiting tool — the brain can break the fast loop.
- event positions = stream synchronization; sqlite = episode record.
- dreaming/keep_alive = between-episode consolidation; prefix-cache planning
  attacks REA's 89M-token deliberation cost.
Missing (new-feature candidates): deadline-aware waiting, stale-observation
handling, confidence-triggered escalation (SFCo-Nav style), budgeted deliberation.

Research shapes:
R1 Gongga as the brain on EFE/RobotAgentBench/CaP-Gym: agent-native exams, measure
   where physical constraints break a general harness (latency, state freshness,
   interruption, cost).
R2 REA-style async routing ON channels: fast policy channel never waits; brain
   intervenes asynchronously; evaluate on ROAR (CPU-only, accessible) or sim.
R3 PDQ-vs-LCRT evaluation protocol for any real-time harness feature.
R4 deliberation cost amortization: prefix cache + dreaming + skill reuse vs the
   43min/89M-token baseline.

Accessibility: ROAR runs on CPU (0.2GB) — the cheapest real-time entry. EFE spans
many simulators (some light, Isaac Sim heavy/GPU). Real robots: no hardware.
Open question: which entry — ROAR (real-time, cheap) vs EFE (embodied, headline).

## Superior's correction (2026-11-15): most "embodied real-time" is quasi-static

User's verdict: moving objects / solving a Rubik's cube are NOT real-time at the
agent's decision layer — every decision moment is a physical steady state; the
non-stationary transients are already handled by low-level PID control. Accepted:
the dividing line is whether the WORLD WAITS for the decision.

- Quasi-static (world waits): manipulation, most of EFE/RAB/CaP-Gym. Latency costs
  time, not correctness. OmniGameArena's PDQ (pause env during inference) is a
  FAITHFUL model of these tasks, not a simplification. The REA brain-cerebellum
  split is the architectural admission: the cerebellum absorbs every transient so
  the brain only ever sees stabilized snapshots.
- Truly real-time (world does not wait): state evolves during deliberation; a late
  answer is a wrong answer. Adversarial games (ROAR LCRT), driving interaction,
  HFT, streaming monitoring. Dynamic manipulation (catch/juggle) has the transient
  AS the task, but it is solved by fast policies (DDPG, online MPC, mutually
  reachable sets — arXiv:2608.26800, 2607.15129), not deliberation; the LLM never
  touches the transient there either.

The open space is deliberation under temporal pressure, and fresh work exists:
- Fresh at Decision, Stale at Action (arXiv:2610.18126): routing new information
  to decisions ALREADY made; models switch 91-100% on decisive news; misleading
  deliveries cost correct decisions; the ledger index sets routing recall.
- Streaming test-time compute (arXiv:2609.14995, Information-Slack Dilemma;
  LiveMind, PredGen, StreamingThinker): thinking while input streams; interaction
  policies + dynamic budgeting.
- Agent Harness Awareness (arXiv:2611.05391): verified telemetry -> operational
  control; observable policy regret measured in milliseconds.
- Execution-grounded SLAs (arXiv:2611.04182): AgentSpec/ToolGate/STRATUS have no
  deadline notion — the gap is named.

Corrected Gongga feature candidates (decision-freshness, not brain routing):
freshness/staleness on decision inputs; deadline-aware anytime waits; a decision
ledger + interrupt-driven revision (send_message already interrupts waiting
tools — half the mechanism exists); think/act scheduling under decaying VoI.
Measurement ground: ROAR LCRT (opponents don't wait; CPU-only). EFE stays a
VERTICAL testbed, quasi-static by design — not real-time research.

