# Agent teams: Gongga vs deepseek-harness, and experiment proposals

Date: 2026-11-17. Session `agent_team`. Worktree `.worktrees/agent-teams`,
branch `feature/agent-teams`.

User's direction: frontier agent-teams experiments; compare against the open-source
DeepSeek harness at `C:/resource/deepseek-harness` (git update asked).

Standing verdict that shapes the proposals (2026-11-15, in
`harness-exp-frontier-survey.md`): implemented features are already validated — no
comparative re-experiments on them; experiments target what the harness does NOT
have, and validated ones become new features.

## State of the deepseek-harness update

- `local/master` == `origin/master` == `687f282e86` (2026-10-17, release 0.2.1-rc.2):
  a month stale. The missing delta is 156,465 objects even for master alone.
- Network measured: SSH (port 22) works but slow (~25–60 KB/s); HTTPS 443 is reset
  both direct and through the local proxies (system slot 127.0.0.1:10793 hangs;
  mihomo 127.0.0.1:7890 gives SSL_ERROR_SYSCALL in 5 s). `flm` not on the bash PATH.
- A targeted `git fetch origin master:refs/remotes/origin/master` runs on channel
  `/dsh` (SSH, steady progress, ~31 MB received at 15:14). The first broad
  `fetch --prune` was killed: it downloaded the same 156k objects plus thousands of
  remote `worktree/*` PR branches nobody needs. Orphaned tmp pack removed.
- The comparison below is against 0.2.1-rc.2, which already contains the full
  agent-team implementation (design note dated 2026-08-05, status: implemented).
  When the fetch lands, diff `packages/experimental/agent-team*` for refinements.

## What deepseek-harness (DSH) has

Sources: `docs/subsystems/agent-team.md`, `docs/subsystems/subagent.md`,
`.agents/notes/implemented/feature/2026-08-05-agent-teams.md`,
`packages/experimental/tool-agent-team/README.md`.

- **Team domain, opt-in.** `TeamId` = the root (Lead) `SessionId`; the team has no
  creation event — durable state begins with the first member/message/task record.
  Flat roster, immutable lowercase-kebab names, a configured member cap. Only the
  Lead spawns or interrupts teammates; any member can message any member and use
  the board. `provisioning -> active | failed` snapshots are durable; a failed
  provisioning permanently consumes its name and slot.
- **Nine model tools, same for every member:** `spawn_teammate`, `send_message`,
  `list_agents`, `wait_agent`, `interrupt_agent`, `team_task_create`,
  `team_task_list`, `team_task_get`, `team_task_update`. Every tool declares its
  complete result schema; results render as compact JSON (compiler-checked, no
  indentation tokens). Team tools shadow the legacy subagent controls of the same
  names; a profile bundle mounts them explicitly.
- **Mailbox with Steer delivery.** A running target receives a peer message at its
  nearest step boundary; an inactive target starts a turn or cold-resumes. The Lead
  log is the transaction home; the target inbox is the acceptance/dedup boundary.
  Historical pending mail is never delivered. `wait_agent` is a bounded polling
  primitive (10 s .. 1 h) that returns `noProgress` when nobody else is running.
- **Shared task DAG.** `TeamTaskSnapshot {id, revision, subject, description,
  status, ownerId, blockedBy, writeScopes}`; `revision` is compare-and-set, stale
  edits rejected; `blockedBy` acyclic; `deleted` is a retained tombstone. Task
  ownership is durable — not released by idle, interruption, or process exit.
  `writeScopes` are **advisory path prefixes, explicitly not locks**: external
  writers (bash, formatters, generators) bypass any fence, and "false mutual
  exclusion is more dangerous than an explicit warning". Views derive readiness
  and write-scope overlap warnings.
- **Shared checkout.** Automatic isolated worktrees were **explicitly rejected**:
  worktree creation, branch naming, merge policy, artifacts and cleanup are
  deployment choices. The final diff and tests remain the Lead's integration
  boundary.
- **Unified subagent activation API, heterogeneous backends:** fork-in-process,
  spawn-in-process, dsh-sdk (separate runtime), **claude-code, codex, acp**.
  `delivery: 'parent' | 'caller'` (notify the model vs just return the result to a
  workflow); `outputSchema` structured JSON capture; `maxDepth` delegation cap;
  `agentPreset` independent composition; `context: fresh | fork` (fork seeds parent
  history); durable child catalog, cold resume, child-first disposal.
- Lead publishes a web projection (`SessionProjectionMap.agentTeam`): roster rows +
  task views + first rejected record. Whole snapshots, append-only, inspectable
  recovery over compact deltas.

## What Gongga has (SPEC + installed skills)

- **Sessions are the agents.** Path hierarchy; any session spawns under its own
  name (`new_subagent`: collision-refusing `new_session` + a `turn_end`
  subscription armed before the brief). Per-session turn gate = truly parallel
  model requests. Model per session drawn by `model_pool` tiers (flash/pro/sota).
- **Event-driven coordination.** `subscribe(pattern, cmds)` runs commands on a
  match, with `{event.answer}` templating; `turn_end` receipts carry the whole
  answer. No polling primitive; waiting is the `wait` tool. `send_message` injects
  at the step boundary and interrupts a waiting tool — the same Steer semantics as
  DSH's mailbox, but server-wide and not team-scoped.
- **Worktree per session** (one writer per worktree) — the opposite of DSH's bet.
- **Context inheritance.** `clone_session` copies a transcript verbatim (fork-like);
  a fresh child's brief is its first message, and `@file`/`@skill` references in it
  enter the prefix-cache index once (fresh-context economics DSH does not have).
- **polity** (skill, not harness domain): justice/advisor/premier approval workflow,
  JSONL records, tier table, one subscribe per role; justice is the user-facing
  session, out-of-loop judge.
- **Lifecycle/memory machinery:** dreaming (idle consolidation clones that write
  workspace documents), keep_alive, live_compact checkpoints. DSH's counterparts
  are the compaction/jobs/schedule/goal/plan packages (not read in detail here).
- Everything in sqlite; one event counter; deltas only to clients.

## The mirror-image bets

| Question | DSH | Gongga |
|---|---|---|
| File concurrency | shared checkout + advisory write scopes (worktrees rejected) | worktree per session (mandated) |
| Coordination state | durable task DAG in the harness (CAS, blockers, owner) | prose in briefs + polity JSONL records in a skill |
| Topology | flat roster, Lead-only spawn, member cap | hierarchical names, any session spawns |
| Notification | `wait_agent` polling + Steer inbox | subscribe event cmds, no polling |
| Team boundary | explicit opt-in domain, branded TeamId | implicit session subtree |
| Heterogeneity | claude-code / codex / acp / separate DSH runtime | homogeneous gongga sessions, any roster model |
| Result shape | `outputSchema` structured capture, declared tool results | `turn_end` answer text + readable transcript |
| Context inheritance | fresh / fork providers | brief + prefix-cache index / `clone_session` |
| Recovery record | whole snapshots append-only in Lead log | sqlite rows + event positions, deltas |

Gongga lacks: a task board, a team-scoped roster/peer mailbox, structured output
capture, heterogeneous backends, a delegation depth cap. DSH (as read) lacks:
event-subscription coordination, dreaming/idle consolidation, prefix-cache index
economics, per-session worktree isolation.

## Experiment proposals (need the user's pick)

All share one prerequisite: a small task suite of multi-part implementation tasks
with cross-part dependencies and **independent grading** (the grader is not the
team's own report — AnalogLayoutWorld's principle; polity's justice stays
out-of-loop). Build it once, reuse across experiments. Results land as data
(`.dev/verify/...`), not prose.

- **E1 — Coordination structure (DSH's core bet, absent in Gongga).** A minimal
  task-board skill: CAS revisions, `blockedBy`, durable owner, advisory write
  scopes + overlap warnings. Same suite with and without the board.
  Pros: squarely a missing feature; skill-level prototype, no server change;
  measurable (stale assignments, write conflicts, integration success, tokens).
  Cons: a skill board is advisory by construction (which is DSH's own argument —
  locks would be false safety); needs the suite first.
- **E2 — Information-sharing policy (Labtune, arXiv:2611.03935, 2026-11).**
  Levels: S0 isolated briefs / S1 turn_end answers only / S2 controller-curated
  recipient views / S3 open blackboard + transcript reads. Gongga's session
  isolation is a free S0; `send_message`/subscribe is the control plane.
  Pros: cleanest independent variable, matches the newest literature, no code.
  Cons: S2 views need discipline or a small mechanism; contamination is harder to
  measure than success.
- **E3 — Topology (Org Science arXiv:2607.25446; AOrchestra).** Polity hierarchy
  (baseline, implemented) vs flat peer team with a mailbox (new) vs
  parent-generated specialists (new). The variable is the new topologies, not a
  re-validation of polity.
  Pros: no code, existing machinery. Cons: confounded with role-prompt quality;
  needs enough tasks to say anything.
- **E4 — The isolation bet, head to head.** Worktree-per-agent (Gongga) vs shared
  checkout + advisory scopes (DSH) on tasks with cross-file dependencies. Two real
  harnesses made opposite, documented bets; nobody has measured this contrast.
  Pros: genuinely novel, both substrates exist on this machine. Cons: DSH must
  build and run (pnpm); merge policy and grader differences add variables; the
  Gongga side is the implemented baseline, the DSH side is the new variable —
  consistent with the verdict, but worth the user's explicit nod.
- **E5 — Heterogeneous team.** Gongga lead + DSH workers driving claude-code/codex
  backends (or the reverse). Cross-harness orchestration is frontier and this
  machine is unusually equipped for it.
  Pros: unique substrate. Cons: most engineering, two failure domains; do after
  E1–E4 say something.

Recommendation: suite first, then **E2** (cheapest, cleanest variable), then
**E1** (first missing-feature candidate), **E4** as the flagship once DSH runs.
E3 folds into E2's runs by varying topology at the same sharing level. E5 later.

## Open questions for the user

1. Which experiment first, and is the recommendation's order acceptable?
2. Task-suite domain: pure coding tasks on this repo, or a verifiable vertical
   (Lean / open-PDK EDA, per `vertical-agent-research-directions.md`)?
3. The git update is downloading over slow SSH (156k objects, a month of delta).
   Keep it running, or is there a faster route (a VPN/proxy mode switch)?