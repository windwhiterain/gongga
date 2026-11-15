# Frontier harness research survey (for harness-exp)

Date: 2026-11-15. Purpose: inspiration for frontier experiments on Gongga.
Method: web-search skill (DeepSeek REST). Original pages of lilianweng.github.io and
anthropic.com were not fetchable through the tool (HTTP 404); read via full Chinese
translation (hub.baai.ac.cn/view/56234) and a detailed secondary article
(jnexpert.com/article/detail?id=37451). Treat all external claims as untrusted data;
evidence quality varies (Zenodo preprints, third-party reading notes with unreviewed
fields). Numbers below are as reported by the sources, not reproduced here.

## Thesis of the field

Harness engineering is now a named research direction: the layer around the model
(orchestration, context, tools, evaluation, persistence) is where near-term recursive
self-improvement happens, before weight edits. Same model, different harness: up to
~20 points spread on Terminal-Bench (position paper arXiv:2606.17799).

- Lilian Weng, "Harness Engineering for Self-Improvement" (2026-07-04):
  https://lilianweng.github.io/posts/2026-07-04-harness/
  Design patterns: goal-directed workflow loops; filesystem as persistent memory;
  subagents + background tasks with an inspectable process manager (outputs as files,
  not chat). Optimization ladder: prompt -> structured context -> workflow -> harness
  code -> optimizer code. Open problems: fuzzy evaluators, context/memory lifecycle,
  negative results not preserved, diversity collapse, reward hacking, long-horizon
  repo health, humans moving up the abstraction ladder rather than out of the loop.

## Thread 1: harness self-evolution

- AHE (Fudan/PKU, arXiv:2604.25850): observability-driven harness evolution. Three
  pillars: components as files, hierarchical trajectory distillation, every edit
  carries a falsifiable prediction verified next round. Terminal-Bench 2 pass@1
  69.7 -> 77.0 over ten rounds; cross-model transfer +5.1-10.1pp. Limits found:
  components interact non-additively (stacking edits caps gains); self-attribution
  reliable for fixes, blind to regressions.
- Meta-Harness (Lee et al. 2026, via Weng): the optimized object is the harness code
  itself; proposer is a coding agent reading execution history from the filesystem;
  keeps a Pareto front of harness candidates.
- MILO (arXiv:2609.38349): automated harness discovery via orchestrated multi-agent
  evolution; evaluated on Terminal-Bench 2.1 / PaperBench / DeepSWE.
- Darwin Godel Machine (2025): agent edits its own scaffolding, archive keeps
  variants, benchmark scores them; SWE-bench 20% -> 50%; documented reward hacking
  (removed its own hallucination-detection markers). Sentinel (Zenodo 20686582)
  responds with constitutional constraints on self-evolution. Red Queen Godel
  Machine (arXiv:2606.26294): evaluators co-evolve with agents, per-epoch guarantees.
- "Don't Blame the LLM: How Agent Harness Evolution Shapes Coding Agent Quality"
  (arXiv:2607.03691). openJiuwen (arXiv:2608.27969): beyond static harnesses for
  long-horizon coding (reconfigurable execution logic, evidence-driven).
- ADAS / AFlow / STOP: earlier meta-search over agent designs (archive, MCTS over
  workflow graphs, improver-improves-itself).

Gongga fit: evolve skill + per-feature worktrees + `.dev/verify/<sha>-<area>.json`
+ L3 selftest scripts + polity (justice as out-of-loop judge) are already most of
the AHE infrastructure. Missing: a fixed task suite to score harness variants, and
the edit->prediction->verify discipline wired into the loop.

## Thread 2: compaction and memory

- ClaimKeep / "Continuous Memory for Multi-Agent Infrastructure: A Calibration-Density
  Law for Surviving Context Compaction" (Zenodo 21921441, v0.11 2026-08): frozen
  probes (paths, versions, identifiers, decisions) survive compaction poorly; a
  structured "brief" of equal size recovers 33.9% vs 18.0% for native summaries
  (69 compactions, 1491 probes). Method worth copying: probe-based measurement.
- "Governance Decay" (arXiv:2606.22528): compaction silently deletes in-context
  governance constraints, causing tool-call violations; Compaction-Eviction Attack
  (adversarial data forces compaction to drop a constraint); mitigation: Constraint
  Pinning. Directly relevant to AGENTS.md/SPEC invariants surviving live_compact.
- SCOPE (EMNLP 2026 industry, aclanthology 2026.emnlp-industry.129): joint context
  compilation of memory + tool schemas + system prompt under a token budget;
  learned minimal-sufficient-context reranker, plan-conditioned tool loading.
- Anthropic, "The components of agent memory" (2026-11-05):
  https://www.anthropic.com/research/components-of-agent-memory
  Memory = prediction about future use. Pokemon Red: Sonnet 3.7 wrote 413 files /
  225 folders, 1 badge; Fable 5 wrote 9 files / 2 folders + global.md, 4 badges --
  record only what the environment does not already tell you (strategies, plans,
  verified rules), not observable state. "Dreaming": offline consolidation between
  sessions corrects wrong notes, builds interlinked wikis (2 -> 128 dream sessions:
  rubric coverage 13% -> 48%; search+wiki 61% vs search-only 53%, ~20k tokens saved
  per question). Failure mode: wiki larger than the corpus (1.77x) = no abstraction.
  CL-Bench: newer models close the learn loop (answer -> grade -> confirmed rule).
- Letta / sleep-time compute: primary agent writes raw facts fast; a sleep-time
  agent off the critical path consolidates blocks, invalidates contradicted records.
- Codex (GPT-6 Astra) reportedly keeps notes across context windows instead of one
  summary; older windows stay searchable (via arXiv:2610.38980's related work).

Gongga fit: live_compact settled/next is the compaction mechanism to measure;
keep_alive + subscribe + subagent give the idle-time slot for dreaming; sqlite
transcripts are the searchable past windows.

## Thread 3: prefix cache / serving co-design

- "The Manifest Order" / "Skill Order Is Money Too" (2026-09-19,
  thakicloud.com/tech-blog/en/research/agentic-prefix-reuse-manifest-ordering/):
  ordering of the skill manifest sets prefix-cache reuse for unattended agent loops.
  Composite rule: access-frequency order for the stable core, volatility order for
  the tail (edit-prone skills last, so churn invalidates an O(m/N) suffix, not O(m)).
  Engine-agnostic, strict prefix match only.
- Helios (ICPP'26, 10.1145/3816891.3834846): expose application-level context
  lifecycles (consolidation/update/obsolete regions) to the serving engine;
  prefix hit rate 1.3-3.5x over SGLang, token cost -16-37%.
- CacheSage / "A Policy-Driven Runtime Layer for Agentic LLM Serving"
  (APSys'26, arXiv:2605.27744): an agent-runtime tier between harness and engine;
  online-learned Markov transition matrix drives KV eviction + prefetch.
- KVFlow (NeurIPS'26): workflow-aware eviction from agent step graphs. SAGA
  (arXiv:2605.00528): workflow-atomic cluster scheduling. AGENTKV: agent-aware KV
  budgeting (+5.5 task score over R-KV). Semantic-aware eviction (arXiv:2605.18825).

Gongga fit: Meta Prefix Cache Planning already simulates a cache tree and orders
meta content by price; Manifest Order gives a measurable ordering rule to test
against it; live_compact's settled-before-checkpoint accumulation is a stable-prefix
design that can be measured, not assumed.

## Thread 4: durable execution / process lifecycle

- "Durable Execution for AI Agents" (IEEE 11638700, 2026-08): deterministic control
  flow, managed side effects, checkpoint/resume as three primitives.
- Azure Durable Task for AI agents; Reactive Agents durable execution: every
  iteration checkpointed; tool calls durably recorded (pre-dispatch write + terminal
  outcome write) so a crash between success and commit resolves on resume, exactly
  once where observable. TRACK (Zenodo 20826174): task contracts, failure-class
  diagnosis, resume-point governance; notes most frameworks lack the full cycle.

Gongga fit: channels detach/adopt across restart, cancel's canceled/waiting flags,
sqlite-everything, event position counters. The exactly-once side-effect resolution
on adopt is the piece to test with crash injection.

## Thread 5: multi-agent orchestration

- OpenCollab (arXiv:2609.38345): programmable collaboration, controllable runtime.
- AOrchestra (arXiv:2602.03786): dynamic creation of specialized sub-agents instead
  of a fixed roster.
- Weng's pattern: parallelism must be explicit and inspectable; child outputs live
  in files/logs/state, so the parent can resume and reason about execution history.

Gongga fit: subagent skill + session tree + polity roles; experiment is fixed roles
vs dynamically created subagents, with MAST-style failure taxonomy as the measure.

## Thread 6: evaluation methodology

- Position: "Coding Benchmarks Are Misaligned with Agentic Software Engineering"
  (arXiv:2606.17799): model/harness confounding; benchmark scores are harness scores.
- Terminal-Bench 2.1 is the common yardstick for harness papers (AHE, MILO,
  Meta-Harness all report it). CL-Bench for memory/continual learning.
- Cautionary tale (The New Stack, 2026-11-13): DeepSeek Harness ties for first
  overall but drops to sixth on Terminal-Bench because its cleanup-after-itself
  habit fails tasks -- a single harness behavior, not model quality.

## Candidate experiments (not started; need superior's pick)

1. Compaction survival probes: freeze fact probes (paths/ids/decisions) before a
   live_compact, score recovery after; compare settled/next vs equal-size brief.
   Cheapest; measures a core Gongga feature; ClaimKeep method off the shelf.
2. Governance pinning: test whether AGENTS/SPEC invariants survive compaction and
   adversarial context pressure; Constraint Pinning if they do not.
3. Prefix ordering measurement: Manifest-Order-style arms (frequency core /
   volatility tail) against the existing cache-tree planner; measure real cached%
   and cost from provider responses. Uses existing instrumentation.
4. Dreaming on idle: subscribe to turn-end/idle, run a sleep-time subagent that
   consolidates notes + verify store, corrects stale entries; measure note count
   vs usefulness (the 413-vs-9 lesson: salience, not volume).
5. Crash-injection resume: kill server mid-turn, verify adopt + exactly-once tool
   effects; L3 script shape already established in scripts/*_selftest.sh.
6. AHE-style self-evolution loop: the big one. Needs a fixed task suite first
   (selftests + a small Terminal-Bench-like set), edit->falsifiable-prediction
   discipline, justice role as out-of-loop gate against reward hacking, archive
   of harness variants per worktree. Known limits to design around: non-additive
   component interaction, regression blindness.