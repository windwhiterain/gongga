# Dreaming on keep_alive

SPEC's `## Dreaming`: the first keep_alive fire of each checkpoint generation clones
the session into a disposable dream that organizes documents only; the clone is
discarded when its turn truly ends. The files it writes are the memory. This note is
the mechanism; the plan and the probe that shaped it are
`.agents/notes/dream-keep-alive-plan.md` and `.agents/notes/dream-probe-report.md`.

## The fire branch

`arm_keepalive` (commands.rs) owns the heartbeat chain: every turn end arms one
sleeper, and a sleeper that fires either dreams or delivers the time notice. The
guards are the heartbeat's own -- a request during the wait (`last_usage_at`) or a
busy session retires the sleeper, because the turn that refreshed the prefix arms
the next one.

A fire dreams when all of these hold:

- the server is not shutting down (a clone in the stop window would only queue its
  notice as an inject; the resumed source turn re-arms the chain after restart);
- the generation differs from the source's `dreamed_checkpoint_id`. The generation
  is the latest `live_compact_checkpoints` id of the source, or 0 for a session that
  never compacted (`Store::latest_checkpoint_id`);
- the source has no live dream (`Store::dreams_of`): at most one dream per session,
  and a running one finishes its generation first.

`spawn_dream` clones, marks the generation on the source
(`Store::set_dreamed_checkpoint`) *before* delivering the notice -- a racing fire
must not dream twice -- publishes the session list, and delivers `DREAM_NOTICE`.
The dreaming fire sends the source no time notice: the dream keeps the cache warm
in its place, carrying the source's whole prefix. Because the source stayed idle,
no turn epilogue re-arms the chain, so the fire branch re-arms it itself. A fire
that does not dream delivers the time notice exactly as before.

## The clone

`Store::clone_session(source, name, Some(Dream { source, generation }))` copies the
transcript verbatim, the stamped system card included: `prompt_in_force` reuses the
stored head while model, cwd and remote match, so the dream's first request carries
the source's prefix and nothing is re-rendered. The name is
`<source>/dream/<generation>`.

The `Dream` overrides, all in the one INSERT:

- `cache_keepalive = 0`: a dream has no heartbeat of its own, so a dream never
  dreams;
- `live_compact = 0`: the opt-in is explicitly off, not left to the home's
  `[agent]` default, so no checkpoint ask ever reaches it;
- `mode = 'work'`, and `deliver` skips the origin checkpoint for a dream
  (`dream_of` on the handle). The plan first said `explore`, but explore refuses
  the write tools (`Tool::writes` in tools/mod.rs) and a dream's whole work is
  writing documents; `work` plus the origin guard gives the dream its tools and
  still opens no checkpoint. This is the one recorded deviation from the plan's
  decision 3, and it keeps the SPEC text ("it opens no checkpoint") intact;
- `dream_of` and `dreamed_checkpoint_id` are written on the dream row too: the
  source it dreams of, and the generation it was spawned for.

`DREAM_NOTICE` (commands.rs) is the dream's whole instruction: organize the
workspace's documents -- consolidate, deduplicate, retire stale notes under
`.agents/notes/` and `docs/notes/` -- no code, no tests, no git writes; the files
are the memory; end the turn when the documents are in order.

## A true end, and the discard

`Agent::run_turn` returns `(TurnOutcome, bool)`: the outcome and the interrupted
flag, captured inside `run_turn` right after `turn_inner` returns. The gate is free
at that point, so a concurrent `deliver` may start the next turn and `new_turn`
clears the flags -- an epilogue read would race it.

The discard sits in the `spawn_turn` epilogue, after `apply_queued_move` and before
the keepalive arming:

```text
discard iff  session.dream_of().is_some()
         &&  (outcome != TurnOutcome::Cancelled || interrupted)
```

Done, Failed and an `interrupt_session` stop are true ends; a resumable Cancelled
is not. `discard_dream` runs the whole delete machinery -- `prepare_delete` /
`finish_delete` -- so channels, background bash and subscriptions go with the row,
and the session list publishes the gone row. The gate is free in the epilogue
(`run_turn` took it by value and `turn_inner` dropped it), so `finish_delete`'s
`claim().await` cannot deadlock; a dream deleting itself mid-turn would.

A cancel that restart will resume is not an end: the dream carries `canceled=1`
(or `waiting_until`) and its owed pending row exactly like any session,
`resume_pending` continues it, and the discard happens only when that resumed turn
truly ends. There is no restart special case.

## Deleting the source

`del_session` expands its doomed list with every row whose `dream_of` names a
doomed id, so a source's dreams go with it through the same machinery -- even ones
a rename left outside the deleted path. The usual path spelling
(`<source>/dream/...` under `<source>`) already covers the common case.

## The orphan sweep

A true-end discard can be lost to process exit: the stop path is a fixed clock, not
a drain of the epilogues (finding F1 of the probe). A dream whose discard died that
way survives as an idle row nothing will ever end again. At startup, after
`prime_sessions` and before anything resumes, `sweep_orphan_dreams` discards every
dream with no pending `turn`/`inject` row and no stop flag (`Store::orphan_dreams`;
no turn runs yet, so none is busy). Dreams with owed rows or flags are resumed,
never swept. For each orphan the source's `dreamed_checkpoint_id` is reset only
while it still marks that orphan's generation (`Store::reset_dreamed_checkpoint`),
so the generation may dream again without undoing a newer one.

A failed dream is discarded too (its turn truly ended); a monitor notice may
already point at the gone row -- accepted deliberately (finding F5).

## Schema and events

`sessions.dream_of INTEGER` -- the source's id, no foreign key: cleanup goes
through the command machinery, not a cascade. `sessions.dreamed_checkpoint_id
INTEGER` -- on a source, the generation last dreamed; on a dream, the generation it
was spawned for. Both are added by `migrate` for an existing home.

The events table carries `dream_spawn` and `dream_discard` (both on the source:
the dream's own events are deleted with its row) and `dream_sweep` (session-less),
so a reader with the database can follow a dream without matching prose.

## Verification

`scripts/dream_selftest.sh` runs the four scenarios on real processes: a fire
dreams and the dream is discarded with the source unharmed; a second fire of the
same generation sends the time notice; a cancel mid-dream resumes after restart
and discards only at the true end; deleting the source discards its dream.