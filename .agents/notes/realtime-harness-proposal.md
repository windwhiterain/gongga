# Real-time harness research proposal: decision freshness (2026-11-15)

Status: DRAFT for superior approval. Grounded in .agents/notes/realtime-probe-report.md
(rt-probe). User's framing: most embodied tasks are quasi-static at the decision
layer (PID absorbs transients); the real research space is decisions where the
world does not wait.

## Research question

When the world evolves during deliberation, which harness primitives recover
decision quality per unit latency — and do they beat adding more deliberation?

Evidence the question is real (ROAR Table 1/4, as reported): Gemini coop score
0.78 turn-based -> 0.13 @250ms -> 0.04 @40ms; an agentic framework HELPS
turn-based (0.78->0.85) but HURTS @40ms (0.04->0.03, latency up to 5880ms).
Shooting domain is far more latency-sensitive than building/cooking (Appendix F).
More thinking is not the answer under deadlines; freshness machinery might be.

## Borrowed theory (what makes this research, not engineering)

- Freshness ceiling (FaD Prop 1): Pr[fact still holds at action] <= exp(-lambda_f
  * Delta_act). No decision-time freshness beats the decision->action gap; only
  news delivered before the revision deadline removes residual risk. => the
  harness must route news to OPEN decisions, not just render fresh context.
- Accuracy decomposition (FaD Eq 3): A = (1-pi)K + pi[rho*S + (1-rho)*L].
  Routing sets rho (recall); judgment sets S (switch when right) / L (hold when
  right). Misleading deliveries cost correct decisions. => measure rho/S/L
  separately; a primitive must show WHICH factor it moves.
- Operational blindness + trust separation (AHA): host measures -> deterministic
  reducer (facts, bounds, freshness, provenance) -> model chooses -> host
  enforces/dispatches. Observable regret in ms = excess operational cost vs the
  best route under the same visible facts. => the measurement vocabulary.
- STTC regimes: grounded-incremental / robust-preparation / speculative; the
  goal is trustworthy on-time response within a declared envelope, not maximal
  early compute.

## The four candidate primitives (all NEW to Gongga; each a SPEC proposal later)

1. Staleness-tagged observations: every observation/event carries its version +
   timestamp; context rendering shows age at decision time; snapshots declare
   freshness bounds. (Gongga has event positions + sqlite timestamps: half there.)
2. Decision ledger + watch-condition routing: a durable record per delayed
   decision (six timestamps, options+alternatives, evidence, dependencies);
   3-8 watch conditions written AT decision time become subscription indexes;
   incoming events match open decisions; decisive news interrupts for revision.
   (Gongga subscribe + send_message-interrupts-waiting-tools: half there.)
3. Revision deadlines as first-class: deadline fires a re-check; no news = zero
   cost hold (no model call); time-effectiveness checks are RULES, not model.
4. Anytime default action: when a decision deadline passes mid-deliberation,
   commit the declared safe default and record effective action frequency;
   missing a tick is a measured event, not a silent no-op.

## Measurement grounds (two, one gated)

G1 (cheap, precise, ungated): FaD synthetic decision stream — generator and data
released with the paper; fictional entities defeat memorization; gold labels
derivable from timestamps+facts; decision->action gaps 3h-3d, revision deadlines
0.5-2h before action. Pure text, our commercial API, exact rho/S/L/K labels.
This ground alone can validate primitives 2+3.

G2 (embodied real-time, GATED): ROAR — text/JSON + structured-audio observation
channel exists (the paper's own heuristic bot is text-only); 7-dim discrete
actions; turn-based / 250ms / 40ms modes; env thread decoupled from agent
threads with observation version numbers. GATE: repo URL not recovered (fetch
drops hrefs; OpenReview forum jrkcJgEwSy has browser verification), license and
Windows build unverified (authors ran Ubuntu 24.04 only). Fallback if the gate
fails: G1 only, plus a minimal MAAG-shaped synthetic loop if needed.

## Phases

P0 gate + setup: recover ROAR repo (OpenReview page via browser-capable tool /
author homepages / PDF footnotes), confirm license + Windows build; obtain FaD
generator. No SPEC changes, no installs outside the project without asking.
P1 phenomenon baseline: our stack, text-only commercial API, turn-based vs
250ms vs 40ms (G2 if gated in; else G1 with compressed clocks): effective action
frequency, score/accuracy, token cost, AHA-style regret. Establish the
degradation curve on OUR harness before adding anything.
P2 primitives: implement 1-4 as Gongga features, each with its own SPEC
amendment proposal + approval; L3 verification per repo conventions.
P3 experiment matrix: baseline vs each primitive vs full, on G1 (+G2); report
rho/S/L/K decomposition, effective action frequency, regret(ms), cost; a
turn-based (PDQ-style) control separates decision quality from latency handling.
P4 analysis: failure taxonomy (stale-at-decision vs routing miss vs wrong
switch vs deadline miss); which primitive moves which factor; paper-shaped
writeup if the effect is real.

## Risks / honesty

- Commercial API P50 1.5-3.2s >> 250ms tick: most ticks no-op, absolute scores
  far below the paper's local small models. BY DESIGN: compare effective action
  frequency and relative recovery, not absolute scores.
- text-only ceiling (0.12 vs 0.18 pixel+text @250ms): acknowledged; the research
  is about latency machinery, not perception.
- ROAR gate may fail; G1 is sufficient for primitives 2+3 and the core claim.
- Cost: ~2122 input tokens/agent-step on ROAR; prefix-cache planning + dreaming
  amortize; G1 is cheap. No budgets set (standing rule); cost is REPORTED.
- All cited numbers are paper-reported, not reproduced here.

## Approval asked

Direction + P0/P1 (probe/setup + baseline measurement; no SPEC edits, no new
features yet). P2 comes back as individual SPEC proposals.
