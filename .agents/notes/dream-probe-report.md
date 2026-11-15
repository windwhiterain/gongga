# Dream probe report — turn-end / restart / cancel semantics

Probe for the "dreaming on keep_alive" feature (plan: `.agents/notes/dream-keep-alive-plan.md`),
questions P1–P7. Read-only: no code changed, no cargo, no server started, no git writes.

Probed at main checkout HEAD `7c948fdf` (2026-11-15). Note: `store.rs` shifted +37 lines
mid-probe when a parallel commit landed (`8d5ecec8`, keep_alive as a channel-row state);
every anchor below was re-verified at `7c948fdf`. That commit touches channel/detached
rows only — none of the turn-end semantics below changed.

## P1 — Every path where a turn ends; does the epilogue run on cancel?

Lifecycle: `deliver` / `claim_recovery` → `spawn_turn` (commands.rs:188) → tokio task →
`Agent::run_turn` (agent.rs:129) → `turn_inner` (agent.rs:213). `run_turn` wraps
`turn_inner` in `catch_unwind` (agent.rs:136-152) and **always returns**; the
`spawn_turn` epilogue (commands.rs:203-265) then runs unless the process has exited.

All turn exits:

| outcome | trigger | anchor |
| --- | --- | --- |
| `Done` | `StepResult::Finished` and inbox empty | agent.rs:298-304 |
| `Cancelled` | `cancel_epoch` changed while queued | agent.rs:221-224 |
| `Cancelled` | session deleted before the turn starts | agent.rs:226-229 |
| `Cancelled` | turn token fired, checked at loop head | agent.rs:252-254 |
| `Cancelled` | `StepResult::Stopped` — token fired mid-step; streamed text kept | agent.rs:307, 1418-1421 (also 1219, 1377) |
| `Failed` | settings/route error | agent.rs:263-266 |
| `Failed` | step error (provider or harness) | agent.rs:315-324 |
| `Failed` | panic, caught in `run_turn` | agent.rs:137-152 |

`run_turn`'s own epilogue (agent.rs:153-211): `open_clear`, answer row, event row,
`finish_turn`, `Event::Status`. Then `spawn_turn`'s epilogue: `read_worktree_state`,
`publish_sessions`, `apply_queued_move` (commands.rs:210-235), keepalive arming
(commands.rs:237-265).

**On cancel the epilogue still runs.** `cancel` fires each busy session's token
(commands.rs:3539); `turn_inner` breaks `Cancelled`; `run_turn` writes the
`turn_interrupted` row, calls `finish_turn(preserve=true)` and returns; the epilogue
continues. Nothing in the cancel path skips it.

**During shutdown, new turns are refused, running ones are not aborted by code:**

```rust
// commands.rs:189-192
if d.shutting_down.load(Ordering::SeqCst) {
    // The turn's pending row stays in the database, so the next start owes it.
    return;
}
```

But there is **no drain wait** for the epilogue of an already-running turn — the
process exits on a fixed clock and drops whatever tasks remain (finding F1). So:
the epilogue is not *skipped* on cancel, it is *usually completed*, with no guarantee.

## P2 — Restart path; the user's recollection is CONFIRMED

Flags written at cancel, per **busy** session only (commands.rs:3519-3536):

```rust
// commands.rs:3530
if let Err(e) = d.store.record_stop(session.id(), until.is_none(), until) {
```

`record_stop` (store.rs:1433-1444) is one exclusive write:
`UPDATE sessions SET canceled = ?1, waiting_until = ?2`. A session mid-turn and not
waiting gets `canceled=1`; one inside `wait` gets `waiting_until`. `interrupt(Stop::Cancel)`
then marks the turn resumable (commands.rs:3444 → session.rs:650) and fires the token.

The interrupted turn keeps its owed row: `preserve = session.take_resumable()`
(agent.rs:196, session.rs:692) and `finish_turn` deletes the pending `turn` rows
**only when `!preserve`** (store.rs:2846, 2874-2881).

The event layer says the same thing — a stopped turn never claims to have ended:

```rust
// agent.rs:178-186
// A stopped turn did not end, so it does not say it did: see server-agent.md.
let kind = match outcome {
    TurnOutcome::Cancelled => "turn_interrupted",
    _ => { row["answer"] = serde_json::json!(answer); "turn_end" }
};
self.log_event(Some(session.id()), kind, &row);
```

`turn_end` is published **only** for `Done` and `Failed` (agent.rs:183), from this one
place. `Event::Status` carries the outcome separately (agent.rs:203-210). The protocol
agrees: "`Done` and `Failed` are a turn the agent finished, and `Cancelled` is a turn
somebody stopped" (crates/protocol/src/message.rs:880-884).

Restart consumes the flags and continues (lib.rs:520 → `resume_pending`,
commands.rs:3757):

- canceled sessions: commands.rs:3873-3888 → `claim_recovery`;
- waiting sessions: the leftover clock is re-armed, commands.rs:3818-3866;
- stranded inbox rows: `nudge_inboxes` (commands.rs:3648) and the same pass.

```rust
// commands.rs:283-284 (claim_recovery)
// SPEC: restart consumes the flag and continues -- this claim is that restart.
if let Err(e) = d.store.record_stop(session.id(), false, None) {
```

**Verdict: the user's recollection is correct at every layer** — event kind
(`turn_interrupted`, not `turn_end`), durable row (kept via `preserve`), and flag
(`canceled` consumed by the next claim, which spawns a continuation). A dream
interrupted by cancel/restart is resumed like any session and must NOT be discarded
there. No restart special case is needed.

## P3 — Recommended discard hook + guard (conclusion)

**Hook: the `spawn_turn` epilogue**, after `apply_queued_move` (commands.rs:~236),
before the keepalive arming. This is the only place that is (a) after the turn's
durable finish, (b) outside the gate, (c) already in the command layer where the
delete machinery lives, and (d) precedented — `apply_queued_move` runs exactly there
(commands.rs:215-235).

**Required change:** `run_turn` currently returns `()` (agent.rs:129-135). Make it
return the outcome **and** the interrupted flag, both captured inside `run_turn`
where they are authoritative — e.g. `(TurnOutcome, bool)`. `TurnOutcome` is `Copy`
(protocol/src/message.rs:885-891). Do not read `session.interrupted()` in the
epilogue: `new_turn` resets it (session.rs:601-602), and a concurrent `deliver`
could start the next turn between `run_turn` returning and the epilogue reading it
(finding F4).

**Guard:**

```text
discard iff  session.dream_of.is_some()
         &&  (outcome != TurnOutcome::Cancelled || interrupted)
```

i.e. discard on `Done`, on `Failed`, and on an `interrupt_session` stop; **never** on
a resumable `Cancelled` (the cancel/restart case). Equivalently: discard iff the turn
wrote a `turn_end` row or was an interrupt. The discard itself must go through the
`del_session` machinery (`prepare_delete`/`finish_delete`, commands.rs:3267-3350),
not `store.delete_session` alone, so channels, background bash and subscriptions go
with the row (P6).

Rejected alternatives:

- **Inside `run_turn`**: the agent has no command-layer delete; it would duplicate
  `finish_delete`'s channel/subscription/worktree cleanup.
- **A `turn_end` subscription**: misses `Cancelled + interrupted` (that turn writes
  `turn_interrupted`), runs asynchronously on the bus, and adds a moving part the
  epilogue hook does not need. If used anyway, it must also match the interrupted
  case, and note `turn_end` fires on `Failed` too (F5).
- **The dream deleting itself via a tool call mid-turn**: deadlocks on the gate (P6).

Placement detail: the dream's `cache_keepalive` is 0, so the chain returns at
commands.rs:238 anyway; the discard can sit just before that check. The fire branch
should also check `shutting_down` before cloning (P4).

## P4 — The keepalive sleeper chain on cancel/restart

The chain is armed in the epilogue (commands.rs:237-265): `cache_keepalive` off →
return; `cache_fire_after_ms` (agent.rs:747-762, TTL × (100 − margin)%) → sleeper
task; on fire, skip if `last_usage_at > ended_at` or `busy()` (commands.rs:250-259),
else `deliver(time_notice())` (commands.rs:261).

- **There is no `shutting_down` check before arming.** A turn cancelled during
  shutdown still arms its sleeper — but the process exits ~250 ms + one 100 ms tick
  later (F1), so the sleeper dies with the runtime. It never persists.
- **If it fires inside the stop window**, `deliver` queues the notice instead of
  starting a turn:

```rust
// commands.rs:1515
if d.shutting_down.load(Ordering::SeqCst) || d.shutdown.is_cancelled() {
    // ... add_pending_task(&name, "inject", &payload) ...
```

- `spawn_turn` itself refuses new turns while shutting down (commands.rs:189-192).
- **Restart:** no sleeper state is persisted; the resumed turn's epilogue arms a
  fresh chain. The fire-branch guards are unchanged.

Consequence for the dream trigger: it rides this same fire branch. If the branch
cloned and delivered during the stop window, `deliver` would queue `DREAM_NOTICE`
as an inject for the next start. Cleaner: **check `shutting_down` in the fire branch
and skip dreaming entirely** — the resumed source turn re-arms the chain after
restart, and the next fire re-evaluates the generation.

## P5 — The clone's stamped card, live_compact state, and `mode`

The environment card is the `Prompt.tail`, rendered once from the TAIL template with
session/build/now/cwd/remote/model (agent.rs:617-634) and stored as a `kind='system'`
message (`append_prompt`, store.rs:1752-1756). On every later turn the stored head is
reused verbatim when the placement and model still match:

```rust
// agent.rs:604-611 (prompt_in_force)
if let Some(kept) = self.store.prompt(id)?
    && kept.cwd == here
    && kept.remote.as_deref() == remote
    && kept.model.as_deref() == Some(model)
{
    return Ok(kept);
}
```

`clone_session` copies **all messages verbatim**, `kind='system'` included
(store.rs:1014-1020), and copies `model`, `cwd`, `remote` unchanged (store.rs:994-1004).
So the clone's first turn reads the source's stamped card and does **not** re-render
it: the card keeps the *source's* session name and the *original* time. Confirmed —
with the caveat that overriding the clone's model/cwd/remote would force a re-stamp.

live_compact state: the clone INSERT lists no `live_compact_*` columns
(store.rs:994-998) and copies no `live_compact_checkpoints` rows, so the clone's
origin/baseline/interval start NULL and its generation is 0/none. **But two planned
overrides are NOT achieved by cloning alone:**

- `cache_keepalive` is copied verbatim (store.rs:998) — a keep_alive source would
  give the dream its own heartbeat chain. T1 must override it to 0.
- the `live_compact` opt-in column (store.rs:1370) is also not copied → NULL →
  follows the home's `[agent]` default, which is not necessarily "off". T1 must
  state it explicitly.
- `mode` **is** copied (store.rs:998; values `work`/`explore`, explore.rs:21-26,37-38,
  default Work at explore.rs:62). A Work source gives a Work clone, and `deliver`
  opens a live_compact origin checkpoint on it (commands.rs:1533-1537 →
  `start_live_compact`, store.rs:2185). Decide the dream's mode deliberately.

Generation helper: the latest checkpoint is `ORDER BY id DESC LIMIT 1` —
`live_compact::bounds` already reads exactly that (live_compact.rs:66-73).

## P6 — Is `del_session` safe from the session's own turn epilogue?

**Yes from the epilogue; never from inside the turn.**

`finish_delete` takes the gate and waits for it:

```rust
// commands.rs:3284-3288
let _gate = match &session {
    Some(session) => Some(session.claim().await),
    None => None,
}
```

- **Mid-turn (gate held)** — e.g. the dream calling `del_session` as a tool —
  `claim().await` waits on the gate the turn itself holds: deadlock until
  `COMMAND_TIMEOUT` aborts the call as a harness bug (commands.rs:27-35). Forbidden.
- **In the `spawn_turn` epilogue the gate is free**: `run_turn` takes the gate by
  value (agent.rs:134) and `turn_inner` drops it on return (`_gate`, agent.rs:213-217).
  Precedent: `apply_queued_move` already runs there via `try_claim`
  (commands.rs:215-235). If a concurrent `deliver` claimed the gate first,
  `claim().await` simply waits — no deadlock.

What the delete takes with it (all needed for a dream): channels ended
(`stop_session`, commands.rs:3291), background bash stopped/detached
(commands.rs:3296-3310), subscriptions forgotten (commands.rs:3315), handle marked
deleted then removed (session.rs:1633-1647), rows deleted (store.rs:1063-1090);
`messages` and `live_compact_checkpoints` go by FK cascade (store.rs:201,
live_compact.rs:119; `foreign_keys ON` at store.rs:900). The epilogue's own handle
clones stay valid; `is_deleted()` guards any later publish. With `cache_keepalive=0`
nothing re-arms for the dead dream.

## P7 — Startup recovery: dreams resume like any session

Startup order (lib.rs:486-523): `answer_open_calls` → `settle_drafts` →
`prime_sessions` → channel adoption → `resume_pending` → `monitor::sweep`. All of
these iterate **every** session row; none filters by kind. A dream mid-turn at
cancel carries `canceled=1` (or `waiting_until`) and is resumed by `resume_pending`
(commands.rs:3818-3888) exactly like any session; it then ends truly and the P3 hook
discards it. **No startup cleanup is needed or wanted for the resume case** —
confirmed. `monitor::sweep` only reports failures, deletes nothing.

One edge remains (finding F2): a dream whose *true-end discard* was cut short by
process exit (F1) survives as an idle session with no pending row and no flag —
nothing ever discards it. It is rare (`Done` racing cancel inside the ~350 ms stop
window). If a startup sweep is added, the safe test is "dream with no pending
`turn`/`inject` row and not busy": a clone whose `DREAM_NOTICE` was queued but never
delivered still holds an inject row and must be woken, not discarded. A process
death between clone and deliver leaves a row-less dream such a sweep would discard;
the source's `dreamed_checkpoint_id` must then allow that generation to dream again
(implementation decision).

## Findings beyond the questions

- **F1 — the advertised active-drain does not exist.** `Dispatch::active`'s doc
  ("`cancel` waits for this to drain", commands.rs:63-67, 184-187) and net.rs:253
  ("The loop left only after the last turn finished") describe a wait no code
  performs: the only touches of the count are `ActiveGuard::new/drop`
  (commands.rs:94,101; subscriptions.rs:188). Actual stop: `cancel` →
  `SHUTDOWN_NOTICE` 250 ms (commands.rs:24, 3572-3576) → `serve` breaks on the next
  `DRAIN_TICK` 100 ms (net.rs:27, 152-158) → lib.rs:661-674 cleanup → exit drops
  remaining tasks. A cancelled turn's epilogue usually completes inside that window
  but is not guaranteed to. This does not weaken the P3 guard (cancel must not
  discard anyway); it produces F2. Worth reporting to the parent as a doc/code drift.
- **F2 — orphan-dream window**, see P7.
- **F3 — clone overrides are real work**: `cache_keepalive` and `mode` are copied;
  the `live_compact` opt-in falls back to the home default. T1's override parameters
  are necessary, not optional (P5).
- **F4 — capture `interrupted` inside `run_turn`**: `new_turn` resets both flags
  (session.rs:601-602); an epilogue read races a concurrent next turn.
- **F5 — `turn_end` fires on `Failed` too** (agent.rs:181-184). Recommended guard
  discards failed dreams as well: the turn truly ended, the artifacts stay on disk,
  and the source dreams again only at the next generation. Note a discarded dream's
  `Error` status may already have drawn a monitor notice; deleting the session makes
  that notice point at a gone row (harmless, but decide deliberately).

## Recommendation (P3 conclusion, restated)

1. `run_turn` returns `(TurnOutcome, bool /*interrupted*/)` — captured inside, where
   they are authoritative (agent.rs:188-196).
2. Discard hook in the `spawn_turn` epilogue after `apply_queued_move`
   (commands.rs:~236): `dream_of.is_some() && (outcome != Cancelled || interrupted)`
   → run the `del_session` machinery on the dream's own name. Restart/cancel never
   discards; the user's verdict holds without a special case.
3. Fire branch checks `shutting_down` before cloning (P4).
4. T1 overrides on the clone: `cache_keepalive=0`, explicit `live_compact` opt-in,
   deliberate `mode` (P5/F3).
5. Optional startup sweep for F2, with the pending-row test above.