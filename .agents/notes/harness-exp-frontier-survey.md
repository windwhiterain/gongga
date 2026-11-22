# Frontier harness research survey (for harness-exp)

Date: 2026-11-15. Purpose: inspiration for frontier experiments on Gongga.

Superior's verdict (2026-11-15): implemented features are already experimentally
validated -- no comparative re-experiments on them. Experiments target features the
harness does NOT have; validated ones become new harness features.

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

Gongga fit: live_compact settled/next is the compaction mechanism; keep_alive +
subscribe + subagent give the idle-time slot for dreaming; sqlite transcripts are
the searchable past windows.

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
meta content by price. Engine-side co-design (Helios, CacheSage) is out of reach
with commercial providers; the harness-side ordering lever is already implemented.

## Thread 4: durable execution / process lifecycle

- "Durable Execution for AI Agents" (IEEE 11638700, 2026-08): deterministic control
  flow, managed side effects, checkpoint/resume as three primitives.
- Azure Durable Task for AI agents; Reactive Agents durable execution: every
  iteration checkpointed; tool calls durably recorded (pre-dispatch write + terminal
  outcome write) so a crash between success and commit resolves on resume, exactly
  once where observable. TRACK (Zenodo 20826174): task contracts, failure-class
  diagnosis, resume-point governance; notes most frameworks lack the full cycle.

Gongga fit: channels detach/adopt across restart, cancel's canceled/waiting flags,
sqlite-everything, event position counters -- implemented and validated.

## Thread 5: multi-agent orchestration

- OpenCollab (arXiv:2609.38345): programmable collaboration, controllable runtime.
- AOrchestra (arXiv:2602.03786): dynamic creation of specialized sub-agents instead
  of a fixed roster.
- Weng's pattern: parallelism must be explicit and inspectable; child outputs live
  in files/logs/state, so the parent can resume and reason about execution history.

Gongga fit: subagent skill + session tree + polity roles exist; dynamic creation
of specialists does not.

## Thread 6: evaluation methodology

- Position: "Coding Benchmarks Are Misaligned with Agentic Software Engineering"
  (arXiv:2606.17799): model/harness confounding; benchmark scores are harness scores.
- Terminal-Bench 2.1 is the common yardstick for harness papers (AHE, MILO,
  Meta-Harness all report it). CL-Bench for memory/continual learning.
- Cautionary tale (The New Stack, 2026-11-13): DeepSeek Harness ties for first
  overall but drops to sixth on Terminal-Bench because its cleanup-after-itself
  habit fails tasks -- a single harness behavior, not model quality.

## New-feature candidates (verified absent; need superior's pick)

Absence checked 2026-11-15 against SPEC, the skill list, and grep over crates/*/src
and ship/skills: no memory store, no dreaming/consolidation, no search/index over
cut transcript segments (get_trajectory is range-based only), no self-evolution
loop, no dynamic subagent creation (subagent skill spawns sessions; polity roles
are fixed). Grep hits for "memory"/"search" were incidental (in-memory state,
provider web_search tools).

1. Memory + dreaming (Anthropic components-of-agent-memory 2026-11-05; Letta
   sleep-time compute). A cross-session memory store with a note lifecycle:
   hypothesis -> verified rule with evidence -> correction/retirement; salience
   discipline (record only what the environment does not already tell you); top
   rules preloaded at session start. An idle-time dream session (subscribe on
   turn-end + the keep_alive slot + subagent) consolidates transcripts/notes,
   corrects stale entries, builds interlinked representations.
   Experiment: dream cycles over this repo's real history; measure stale entries
   corrected and later-session retrieval; guard the 413-vs-9 over-recording trap.
2. Self-evolution loop (AHE arXiv:2604.25850; Meta-Harness; MILO; DGM). Harness
   variants as worktree branches; every edit carries a falsifiable prediction
   verified next round; a fixed task suite scores variants; the archive keeps
   winners AND negative results; justice stays out of the loop as the gate.
   evolve deploys today, but nothing proposes/scores/archives variants. Start
   with content-repo components (skills/prompts, no rebuild) before server code.
   Design around the known limits: non-additive interaction, regression blindness
   (held-out suite), reward hacking (evaluator outside the loop).
3. Recall over cut history (Codex Astra-style notes across windows). Index cut
   transcript segments; an agent-facing recall tool returns snippets with
   positions; live_compact summaries carry recall handles instead of everything.
   Experiment: a long task with forced compaction, probes whose answers live only
   in cut history; does recall recover them.
4. Dynamic subagent creation (AOrchestra arXiv:2602.03786). The parent generates a
   specialist (role prompt + model config + tools) per subtask instead of fixed
   polity roles; child outputs as files. Experiment: fixed roles vs generated
   specialists on a multi-part task.
5. Constraint pinning (Governance Decay arXiv:2606.22528). Pinned context rows
   (SPEC invariants, user verdicts) that compaction cannot cut. Small; can fold
   into the memory store as pinned entries.

Excluded by the verdict: re-measuring live_compact survival, prefix ordering vs
the existing planner, crash-resume of adopt -- all implemented and validated.

Recommendation: 1 first (self-contained, builds on existing primitives), 2 as the
flagship (needs a fixed task suite first), 3 as the companion to live_compact.

## Later thread bearing on items 3 and 5 (added 2026-11-22 by a dream)

The user opened a separate direction on 2026-11-22: a per-request **KV mask** over an
append-only tape, positions unchanged, validated on a local engine. It is not a vertical
agent; it is the inference/context layer under all of them. Recorded in
`2026-11-22-a-kv-mask-is-a-view-not-an-edit.md`,
`2026-11-22-a-hybrid-state-is-a-free-ghost.md`,
`2026-11-22-append-is-the-only-primitive.md`, `kv-mask-experiment-plan.md`
(direction settled by user verdicts; nothing built, no script permission yet).

- **Item 3 (recall over cut history)**: with a mask tape, recall is an **in-place unmask**
  -- order and positions intact, no re-prefill -- instead of appending a snippet at the end
  as a tool result. That is a strictly better shape for the same experiment. On a hybrid
  model it is exact: the recurrent state is never touched, so unmask restores the original
  view rather than re-anchoring bytes at a new position.
- **Item 5 (constraint pinning)**: an existing wheel exists on the engine side -- EVOKE
  exposes harness-supplied `evoke_pinned` / `evoke_priority` tags. Note the user rejected
  building on EVOKE's primitives (they mutate the cache; the proposal is a view), so this
  is a pointer to prior art, not a dependency.
- The survey's own method note still governs: paper numbers are self-reported and
  untrusted; both KV-mask papers state their scripts were AI-written.