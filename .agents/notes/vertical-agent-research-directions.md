# Vertical-agent research directions (discussion, 2026-11-15)

User's shift: coding agents feel near-saturated; move the research goal to vertical
(domain-specialized) agents. Note for the record: coding-harness research is still
yielding gains (AHE +7.3pp on Terminal-Bench 2; the DeepSeek Harness cleanup-habit
incident), but the marginal novelty is indeed moving to verticals, and the harness
mechanisms transfer. Dreaming/memory (in implementation now) is the bridge.

## Frontier map (web-search, 2026-11-15; claims as reported, untrusted)

Hard-engineering verticals (verifiable, hours-long, hottest cluster):
- PDAgent-Bench (arXiv:2606.17253): VLSI physical design loops with tool feedback;
  11 models characterized; limits = long-horizon reasoning, tool interaction,
  cross-stage coordination.
- AnalogLayoutWorld (arXiv:2611.16991, 2026-11-11): open agentic analog-layout
  benchmark; network-isolated containers; INDEPENDENT grading (DRC/LVS/PEX/post-
  layout sim); agent-reported results do not determine the score.
- Long-Horizon Analog Design Bench (arXiv:2609.33356): hours-long AMS tasks.
- Chip timing closure (Zenodo 22019953): cross-STAGE context propagation in EDA.
- RefEvo (arXiv:2604.24218): co-evolutionary verification; "Coupled Validation
  Failure" — both the design and its testbench can be wrong.
- UCSD workshop (Towards Cognitive Silicon Design, 2026-04): benchmark dilemma —
  saturation/memorization, no eval standards, opaque failure modes.

Self-driving labs:
- AutoDNA (SJTU, Nature 2026-11-04): AI-native wet lab; multi-agent decomposition
  (literature -> reagents -> protocol -> code -> hardware -> analysis -> iterate);
  instruments as "atomic services" (natural-language capability + unified code
  interface); scheduling under timing/operation constraints.
- Google VERID (research.google blog, 2026-10-15): digital twin of the bench
  (arm position, well volumes, tip inventory), re-verify state BEFORE acting,
  video + action logs for audit; closed-loop wet lab; control experiments caught
  a false positive in myoglobin screening.
- Bayesian vs agentic optimization testbed (arXiv:2611.11690): minimal closed-loop
  color formulation; the "next experiment" policy is the research variable.
- Adaptive spectral analysis (arXiv:2610.25619): deciding when a measurement can
  yield a reliable value and when it cannot.

Clinical:
- MedAgentBench v2 (PSB 2026): FHIR EHR; gains from structured tools + calculator
  + a MEMORY component that learns from prior failures.
- Rethinking MedAgentBench (OpenReview j92gdh0VRH): grading bugs (bare-string
  equality vs FHIR CodeableConcept) — evaluation fairness is itself research.
- NEJM AI review (AIdbp2500144): CRAFT-MD, AgentClinic, metacognition gaps.

Cross-cutting:
- Manifold (2026-08-12): in life sciences the marginal return on harness work
  exceeds model work.
- Deep-research evaluation: process-level (evidence acquisition), not just final
  answers (preprints 202608.0572); DRACO cross-domain benchmark.

## Research directions that fit Gongga's machinery

R1 Domain memory loop — the dreaming feature's first vertical test. Question:
   does offline consolidation of domain experience (verified rules + evidence,
   correction of stale ones) raise later-task success? MedAgentBench v2 shows
   memory-from-failures works in clinical; Anthropic shows salience discipline.
   Gongga substrate: dreaming (in impl), sqlite transcripts, skills.
R2 Hours-long experiment loops — channels (detach/adopt, wait/until) + bounded
   context (live_compact) as the process manager for simulation/experiment loops
   (MarkovSim shape: 89% on FEA with bounded context; Analog Bench is hours-long).
   Cross-stage context propagation (EDA timing closure) maps to checkpoints+meta.
R3 Verifier independence & co-evolution — RefEvo's coupled validation failure vs
   AnalogLayoutWorld's independent grading vs DGM reward hacking. Question: what
   harness mechanism keeps verification honest when the agent writes both sides?
   Gongga substrate: polity justice out-of-loop, separate eval containers.
R4 Harness transfer measurement — same model, general harness vs domain harness
   on a vertical benchmark, mechanism ablations. The position paper's confounding
   argument applied to verticals. (Careful: this measures transfer, not re-
   validating implemented features — consistent with the user's verdict.)
R5 Evidence chains & pre-action state checks — VERID/ScientistOne shape: every
   claim traceable, state re-verified before physical/irreversible actions.
   Gongga substrate: everything-in-sqlite + event positions; new invariant for
   domain actions.

## Entry requirements

Pick ONE open, verifiable vertical first: EDA with open PDK (sky130 + OpenROAD/
magic/ngspice; AnalogLayoutWorld is open), theorem proving (Lean: perfect
verifier, zero reward-hacking surface), or open-source simulation (CalculiX/
OpenFOAM; MarkovSim shape). Then: domain skill pack (instruments/tools as atomic
services = Gongga skills), a small task suite with INDEPENDENT grading, then R1/R2
experiments on it. Wet labs and clinical EHRs need resources we do not have.

Open question for the user: which vertical, and what tool access exists.