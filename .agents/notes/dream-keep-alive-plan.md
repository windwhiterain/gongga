# Dreaming on keep_alive — plan (harness-exp)

Feature: the first keep_alive fire of each live_compact checkpoint generation clones
the session into a disposable "dream" session that organizes documents only; the
clone is discarded when its turn truly ends. The files it writes are the memory.

## Superior's verdicts (recorded)

- 2026-11-15: implemented features are already validated; experiments target only
  features the harness does not have. Dreaming/memory is the chosen new feature.
- 2026-11-15: mechanism fixed by the user — ride on keep_alive; the FIRST
  keep_alive of a checkpoint clones a session, tells it to organize documents
  only, discards it when done.
- 2026-11-15: **restart is NOT a turn end**. No restart special case: a dream
  interrupted by cancel/restart rides the existing recovery semantics and is
  discarded only when its turn truly ends. The earlier "restart discards
  unfinished dreams" clause is WITHDRAWN.
- 2026-11-21: **the second countdown starts at the dream's end, not its spawn**.
  The user's verdict: during a dream the cache stays hot, because the dream carries
  the source's whole prefix. Arming the second fire when the dream spawned handed
  off while the cache was still warm. The source now owes no heartbeat while a
  dream is live; the dream's true-end epilogue arms the second countdown after the
  discard. This changes the earlier fire-branch re-arm in `arm_keepalive`.
- 2026-11-20: the heartbeat was observed never to fire. Fixes: read tuning in
  the turn epilogue, use the session's model for the TTL, arm on
  `set_session --keep_alive true`, and re-arm idle sessions at startup.
- 2026-11-20: **same generation, two stages**. First fire dreams; second fire
  asks for `handoff`; after that mute every heartbeat until a new user prompt.
  The handoff's own note arrives from a tool and does not unmute -- only a
  client-origin `send_message` does.
- 2026-11-15: this session plans and reviews; subagents probe and implement.
- 2026-11-15: "批准" — SPEC.md amendment (T5) and the L3 script (T4) are both
  approved. SPEC text finalized by the planner, sent to impl verbatim.

## Probe conclusions (harness-exp/probe, report: dream-probe-report.md)

The user's recollection is CONFIRMED at every layer: `turn_end` is published only
for Done/Failed (agent.rs:178-186); Cancelled writes `turn_interrupted` and keeps
the owed pending row via `preserve`; restart consumes the flags and resumes
(claim_recovery). The epilogue DOES run on cancel, so the discard needs a guard.

Final decisions (planner, after probe review):

1. `run_turn` returns `(TurnOutcome, bool /*interrupted*/)` captured INSIDE
   run_turn (F4: epilogue reads of session flags race the next turn).
2. Discard hook: spawn_turn epilogue, after apply_queued_move, before keepalive
   arming. Guard: `dream_of.is_some() && (outcome != Cancelled || interrupted)`.
   Discard on Done, Failed, and interrupt_session stops; NEVER on resumable
   Cancelled. Goes through the full del_session machinery
   (prepare_delete/finish_delete), never store.delete_session alone (P6: gate is
   free in the epilogue; mid-turn self-delete deadlocks).
3. Fire branch: check shutting_down before cloning (P4). generation = latest
   checkpoint id or 0. Dream when generation != dreamed_checkpoint_id and no
   live dream of this source exists. Clone name `<source>/dream/<generation>`.
   Overrides: cache_keepalive=0, live_compact opt-in explicitly OFF, mode
   'explore' (a Work-mode deliver opens a live_compact origin; a dream opens no
   checkpoint). Re-arm the source's next fire after spawning (chain must live).
4. DREAM_NOTICE: fixed English instruction — organize documents only
   (.agents/notes/, docs/notes/: consolidate, dedup, retire stale), no code, no
   tests, no git writes; the files are the memory; end the turn when done.
   No budgets or limits (forbidden by the standing rules).
5. Deleting the source discards its dreams through the same delete machinery.
6. F2 orphan sweep at startup: discard dreams with NO pending turn/inject row
   and not busy (their true-end discard was lost to process exit, F1); reset the
   source's dreamed_checkpoint_id so that generation may dream again. Dreams
   with pending rows or flags are resumed, never swept.
7. Schema: sessions.dream_of INTEGER (plain, no FK cascade — cleanup goes
   through the command machinery), sessions.dreamed_checkpoint_id INTEGER;
   migrate() adds both (live-tracked invariant).
8. F5: failed dreams are discarded too (the turn truly ended); a monitor notice
   may point at the gone row — accepted deliberately.

F1 (doc/code drift: the advertised active-drain wait does not exist; stop is a
fixed ~350 ms clock) is reported to the user, OUT OF SCOPE for this feature.

## Anchors (re-verified by the probe at 7c948fdf; main moved mid-probe: 8d5ecec8
landed from parallel work, channel-row keep_alive only, turn semantics unchanged)

- Fire point: commands.rs spawn_turn epilogue ~237-265; cache_fire_after_ms
  agent.rs:747; deliver commands.rs:1502 (queues inject when shutting_down).
- Turn exits: agent.rs:298-324; run_turn wrapper agent.rs:129-211; event kind
  choice agent.rs:178-186; protocol message.rs:880-891.
- Delete machinery: commands.rs:3267-3350 (finish_delete takes the gate).
- clone_session store.rs:947+ (copies messages verbatim incl. the stamped system
  card; prompt_in_force agent.rs:604-611 reuses it — no re-stamp while
  model/cwd/remote match; live_compact columns NOT copied; cache_keepalive and
  mode ARE copied).
- Recovery: lib.rs:486-523, resume_pending commands.rs:3757+; record_stop
  store.rs:1433.

## Implementation breakdown

- T1 store: columns + migrate; clone override parameters; latest-generation
  helper; source-delete discards dreams; F2 startup sweep.
- T2 commands/agent: run_turn outcome return; fire branch; discard hook;
  DREAM_NOTICE; re-arm.
- T3 docs/notes/server-dream.md.
- T4 scripts/dream_selftest.sh (L3) — APPROVED; repo selftest conventions, the
  four scenarios, must actually run green.
- T5 SPEC.md amendment — APPROVED; the Dreaming section below, verbatim, placed
  after the Live Compact section; nothing else in SPEC changes.
- Verification: build + fmt + comments-check vs baseline + test-layout-check,
  the L3 script, interactive evidence for the cancel/restart scenario.
- After planner review: merge at checkpoint, evolve to take over the home.

Worktree: `.worktrees/dream-keep-alive`, one writer: the impl subagent. Planner
reviews from the main checkout.

## SPEC amendment (approved text, verbatim)

```
## Dreaming
rides on keep_alive: the first fire of each checkpoint generation clones the session
into `<session>/dream/<generation>`, tells the clone to organize documents only, and
discards the clone when its turn truly ends.
- the clone copies the transcript verbatim and re-renders nothing, so its first request
  carries the session's whole prefix: the dream keeps the cache warm in place of the
  empty time notice.
- the clone's keep_alive is off and it opens no checkpoint; a dream never dreams. one
  dream per generation, at most one live dream per session; a fire that does not dream
  sends the time notice as before.
- a true end is Done, Failed, or an interrupt; a cancel that restart will resume is not
  an end -- the dream resumes with the session and is discarded only when it finishes.
- the dream writes workspace documents and nothing else; the session is disposable, the
  files are the memory. deleting the source discards its dreams.
```