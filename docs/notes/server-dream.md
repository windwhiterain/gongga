# Dreaming on keep_alive

SPEC's `## Dreaming`: the first keep_alive fire of each checkpoint generation clones
the session into a disposable dream that organizes documents only; the second fire of
that generation asks the session to hand off. The clone is discarded when its turn
truly ends, and no later heartbeat reaches the source until a new user prompt. The
files the dream writes are the memory. This note is the mechanism; the plan and the
probe that shaped it are `.agents/notes/dream-keep-alive-plan.md` and
`.agents/notes/dream-probe-report.md`.

## The fire branch

`arm_keepalive` (commands.rs) owns the heartbeat chain: every turn end arms one
sleeper, and a sleeper that fires dreams, asks for handoff, or delivers the time
notice. The guards are the heartbeat's own -- a request during the wait
(`last_usage_at`), a busy session, a tuning that turned the heartbeat off, a change
of generation, or a change of mute state retires the sleeper: the turn that
refreshed the prefix arms the next one.

The arm reads tuning at arm time, not before the turn. A turn may enable
`cache_keepalive` or change the model; the epilogue reads the session's current
`tuning` and uses that session's model for the provider's TTL, not the server's
default. `set_session --keep_alive true` arms an idle session immediately, because
no later turn is owed, and startup arms idle sessions with the switch still on
(`arm_idle_keepalives`, after pending turns are claimed). A busy session is left to
its own epilogue.

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
in its place, carrying the source's whole prefix. The fire arms no timer. A fire
that cannot dream because the clone failed, or because a live dream is still
running while the generation is unmarked, keeps the time notice as before.

## The countdown starts at the dream's end

The dream holds the source's whole warm prefix for its entire turn, so the cache
cannot expire while it runs. Starting the second window at the dream's spawn
would hand off while the cache is still hot. The source therefore owes no timer
while a dream is live: `arm_keepalive` refuses a source with a live dream, and a
second sleeper armed before the dream spawned retires itself at fire time.

The dream's true-end epilogue calls `arm_source_after_dream` after
`discard_dream`. It resolves the source by id -- a dream may have been renamed
while it ran -- and arms from that moment. The window measured at that arm is
the first one after the cache actually went cold. A cancelled dream that restart
will resume is not an end, so it discards nothing and arms nothing; the resumed
turn's own true end does both. Startup and `set_session` cannot arm a source
whose dream is still live: `arm_idle_keepalives` and the `set_session` arm use
the same guard.

## The second fire: handoff

The marker is the source's `dreamed_checkpoint_id`. Once it equals the current
generation, the first stage is spent: the next fire -- armed at the dream's true
end, not its spawn -- does not dream and does not send a time notice.
`spawn_handoff` writes
`sessions.keepalive_handoff_at`, logs a `keepalive_handoff` event, and delivers
`keepalive_handoff_notice` -- prose from `[prompts]`, with a fallback for content
that predates the template. The mark is written first, so the notice's own turn
finds the chain muted and its epilogue arms nothing.

Mute is durable. A restart re-arms only unmarked sessions; a handoff's own note
arrives through `send_message` from a tool and does not clear the mark. Only a
client-origin `send_message` -- a new user prompt -- calls
`Store::clear_keepalive_handoff`. A tool's message, a monitor's report and a
mentor's answer are all messages too, and none of them is a user prompt. A new
checkpoint also retires an older sleeper: a generation change is a new fire
chain.

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

The discard sits in the `spawn_turn` epilogue, after `apply_queued_move`; arming
the source's next countdown follows it:

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
was spawned for. `sessions.keepalive_handoff_at INTEGER` -- when a heartbeat last
asked this session to hand off; non-NULL is the mute. All three are added by
`migrate` for an existing home. A clone starts unmuted: the mark belongs to a
lineage, not to a session name.

The events table carries `dream_spawn`, `dream_discard` and `keepalive_handoff`
(all on the source: the dream's own events are deleted with its row) and
`dream_sweep` (session-less), so a reader with the database can follow a dream
and the escalation without matching prose.

## Verification

`scripts/dream_selftest.sh` runs the scenarios on real processes: a fire dreams
and the dream is discarded with the source unharmed; a second fire of the same
generation asks for handoff and then stays mute until a client message; a cancel
mid-dream resumes after restart and discards only at the true end; deleting the
source discards its dream; and a startup sweep discards an orphan whose discard
was lost to a process exit. The handoff case compares event timestamps: the
`keepalive_handoff` row must be later than the `dream_discard` row, so a countdown
that started at the dream's spawn -- before the cache went cold -- fails it.