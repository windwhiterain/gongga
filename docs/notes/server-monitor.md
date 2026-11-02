# server-monitor: the session that is told to fix what stopped with an error

SPEC: "once a session stopped with error, auto spawn a moniter session told to
fix. moniter session will be choosen from usable flash, auto switch if it also
errored".

Three decisions carry the whole of it: what a failure *is* (data, not an event),
which session it goes to (`#monitor`, one of them), and how that session is
chosen and re-chosen.

## A failure is a row, not an event

The trigger is `sessions.status = 'error'`, read from the database:

```
status = 'error' AND monitor_seq < the session's newest message seq
```

A subscription to the bus would have been more immediate and less correct: a
broadcast subscriber is told it `Lagged`, and a turn that ended while this process
was not running published nothing at all. The status column survives both, and it
is the same field the window paints a session's failure from, so there is one
answer to "did this session fail" rather than two.

`monitor_seq` is the last row a monitor turn was given, and `sessions.monitor_attempts`
is how many attempts that search has made for this failure. No new table:
a turn writes its user row before it starts (`store::begin_turn`), so `seq` only
grows, and "this transcript has been handed over" is one comparison.

Two callers run the same `monitor::sweep`: start-up, before the port binds, and
the two-second tick that already wakes inboxes. The answer therefore does not
depend on which of the two noticed a failure first.

Only the *handover* is recorded, and it is recorded after the message is accepted:
a crash between the two repeats a report, which is recoverable, where the other
order loses one.

## One monitor session, under a name nobody else may take

The session is `#monitor`. Built-in sessions carry a leading `#`, and
`set_session` refuses that prefix with a sentence saying so. This is why the
reservation is worth having at all: the harness never has to look at a session and
decide whether it is its own, and a person's session called `monitor` -- which
2026-11-02 showed is a name people do use -- is simply their session. Deleting
`#monitor` is allowed, because a monitor that cannot work has to be removable; the
next failure creates it again.

The session is created with no `--cwd`, so `SessionManager` gives it the server's
own directory, exactly like a session made from a window.

Delivery is `commands::deliver`, the same path `send_message` takes: idle means a
turn starts now, busy means an inbox row the next step boundary reads. The queue
gate, `@skill:` expansion, shells, tools, events, and the window's rendering are
all reused; there is no second way to start a turn.

## Choosing the model: usable flash

`monitor::candidates` is the roster's `tier = "flash"` models that this server
could actually send a request on: the roster places it, a credential resolves for
it, and it takes the Responses API. The last one is `Catalog::speaks_responses`,
the same test `default_entry` applies -- without it a roster entry the shape cannot
drive would be picked and then fail with an error that says nothing about tiers.

Order: the worst window a provider last reported, best first; providers that have
reported nothing sort after the ones that have, in roster order. "Worst window"
because a provider with a full month and an empty five-hour window cannot serve
the next turn, and a wallet with no window is a nominal share so it competes
rather than being shut out. This is the same `fail_open` reading the `model_pool`
skill uses: nothing reported is not a refusal.

That comparison is why `quota` keeps a reading per provider. `quota::last()` is
still the newest reading of any provider, which is what SPEC's `active_quota` row
is about; `quota::of(provider)` is what a choice between providers needs.

The chosen model is written to `sessions.model` and the session's `Tuning` before
the report is delivered, so the turn cannot start on the model that just failed.

## When the monitor itself fails

The monitor is an ordinary session, so its own failure is an ordinary
`sessions.status = 'error'` row -- `self_failed` is `name == SESSION`, which is
now a fact rather than a guess.

`sessions.monitor_attempts` counts the attempts made for this failure, so it is
also the index of the *next* candidate. On a monitor failure the sweep takes
`candidates[attempts]`, writes it, and delivers **the report the monitor was last
given** (`store::user_texts`, last row) with a preface naming the model that failed
and what the server recorded for that turn. Re-deriving the report would lose what
the first attempt was told.

Two details that are only obvious after getting them wrong:

- A monitor created *during* this handover inherits the attempt count
  (`store::set_monitor_attempts`), because otherwise its first failure would be
  read as attempt 0 and the model that just failed would be chosen again.
- `store::finish_turn` clears `monitor_attempts` and pushes `monitor_seq` past the
  transcript whenever a turn ends *not* in error. A session that recovers and
  fails again has a new episode, and the search starts from the best candidate.

When the list is exhausted the sweep writes one `Event::Log` and stops: `#monitor`
has failed on every usable flash model, and the session in error is left for a
reader. No recursion, no invented retry budget -- `AGENTS.md` forbids one, and the
failure is still visible in the window either way.

## What the report says

Enough to start work without reading anything first: the session and its model,
the row range, the harness build (`protocol::version_line`), the home, the server
pid and cwd, the model this turn runs on, the last `notice` row the server wrote
for that session (which is where `Agent::error` puts the reason), and the exact
command that reads the transcript back. Plus the two things worth doing:
`@skill: self-control` to restart the server after a fix, and `set_session` when
the cause is a credential or a quota.

Nothing about it is reserved for a skill: the report is built by the server, so
it is the same whether or not this home has any skills installed.

## The boundary with `model_pool`

The server reads the roster's `tier` and the providers' own quota readings.
`preference.toml` weights, `model_pool mark`, and the entitlement probe stay in the
skill, because they are this machine's policy and not the harness's. Running the
skill's python from the server would also leave a wrong answer on a machine where
the skill is not installed, which is the "two implementations" this avoids by not
starting.

## Verified

`scripts/monitor_selftest.sh`: scratch home, ports the kernel picks, a fake
Responses endpoint that refuses one model id (`FAKE_LLM_FAIL_MODEL`), and a roster
of two flash models where the first is the refused one -- so one run covers the
whole chain:

| step | result |
| --- | --- |
| `set_session --session '#mine'` | refused: the `#` prefix belongs to the harness |
| a session on the refused model | `turn end … outcome=Failed`, `status='error'` |
| the handover | `handed a failure to the monitor session=broken model="dead/flash-dead"`, `#monitor` created, `monitor_seq` past the transcript |
| the monitor's own turn | `turn end session=#monitor outcome=Failed`, then `model="fake/flash-fake" attempt=2`, and that turn ends `Done` |
| the retry's transcript | the original report, unchanged, behind a preface naming the failed model |
| six more seconds of ticks | no second report for the same failure |
