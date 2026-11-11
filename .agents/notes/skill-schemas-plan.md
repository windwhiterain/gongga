# Skill schemas + nested SessionConfig — plan & decisions

Worktree: .worktrees/skill-schemas (branch feature/skill-schemas, base e1fcab59).
Orchestrator: prompt. Design confirmed by user verbatim (mentor_2 seq 869-931).

## Confirmed design (user's words, mentor_2)
- seq869: ls_skill/run_skill schema -> new ship skill "skill"; other server cmd schemas -> self-control; subagent no longer references self-control, exposes new_subagent + ask.
- seq912: self-control provides NO convenience functions, only faithful server-cmd schema docs; subagent provides convenience (copy own config, flash/pro/sota random pick).
- seq919: spawn AND new_session take SessionConfig; add self_config helper to GET own SessionConfig; reference model_pool; model_pool provides a ModelConfig-picking function; ask_session stays in self-control (new_subagent already subscribes).
- seq930: 1.self_config=GET own config 2.nested=yes 3.modify community=yes.
- seq931: dispatch subagents (model_pool MUST be a subagent).

## Implementation decisions (orchestrator, within confirmed design)
- Wire: Args=BTreeMap<String,String> (flat). Nested SessionConfig travels as ONE arg
  `config` = JSON string. No protocol Args type change. new_session/set_session args:
  id (session,sticky,required) + config (JSON SessionConfig, optional) + mode (optional, kept).
- config optional: new_session omitted => defaults; set_session omitted => no change.
- set_session delta = deep merge (omitted keys preserved at every level).
- mode kept as top-level optional arg (NOT in SPEC SessionConfig; preserves explore/work
  used by live compact). must_create stays internal to new_session (forced true), not a config field.
- SessionConfig shape (SPEC): {model_config:{model:{provider,model},thinking,ctx_limit,cache_tier},
  workspace:{remote,cwd}, keep_alive}.
- model_pool API contract (fixed for both briefs): keep existing pick (model string) for compat;
  ADD pick_config(tier=...) -> ModelConfig dict {model:{provider,model},thinking,ctx_limit,cache_tier}.
  subagent calls pick_config. (User wrote model_pool.pick; pick_config satisfies intent without
  breaking existing pick callers.)
- self_config(): read caller's sqlite row (COLUMNS like self-control/new_session.py), reshape to
  nested SessionConfig. Read-only reshape, no duplication of server write logic.

## Components & dispatch
1. server new_session/set_session nested config — ME (prerequisite, riskiest). crates/server.
2. subagent skill: new_subagent(config)+self_config() — subagent C (after 1 + model_pool API).
3. model_pool pick_config — subagent A (community repo, PARALLEL, independent). MUST subagent.
4+6. self-control = schema docs + ask_session; delete new_session.py — subagent D (after 1).
5. skill skill (ls_skill/run_skill docs, body-only) — subagent B (independent).

## Worktree / write discipline
- A works in community clone ~/.agents/gongga-agents (symlinked into general bucket => live).
- B/C/D ship-content: run SEQUENTIALLY in main worktree (one writer at a time), each commits to
  ship local; orchestrator publishes ONCE via scripts/ship.sh -m at the end. Avoids ship races.
- Server code: commit to feature/skill-schemas, then evolve (do NOT restart user's running server).
- Verification: real-process scripts/*_selftest.sh (build gongga-server AND gongga-relay).

## Known independent bug (report/fix later, ship content)
- heir transcript.py send_lesson: Windows python subprocess calls GONGGA_CLI (a /bin/sh shim)
  => WinError 193. Lesson falls back to tool_result. Fix: resolve a Windows-executable command
  layer. Needs scripts/heir_selftest.sh before ship. Out of scope unless time permits.

## Dispatch status / blocker found
- BLOCKER: the tool-backed `skill` cmd route on the RUNNING build (04c80735, older than HEAD)
  rejects calls with "skill is missing required argument(s): session" even when session is
  provided (model_pool and subagent spawn both). Direct server cmds work fine.
  => Dispatch subagents via DIRECT cmds, not `skill subagent spawn`.
- Workaround helper: .dev/dispatch.py <role> <cwd> <model> <brief-file>
  (set_session must_create + subscribe ^turn_end:<name>$ -> send_message parent + send brief).
- CLI helper: .dev/gg.py <cmd.json>  (build JSON in python; bash $GONGGA_CLI; print cmd_result).
- Worker A dispatched: prompt/modelpool (deepseek-official/deepseek-flash), cwd community clone.
- TODO server: investigate skill-route session binding (may already be fixed at HEAD; verify after evolve).
