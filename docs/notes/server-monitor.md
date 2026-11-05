# server-monitor: the sessions the harness spawns and hands a report to

SPEC: "once a session stopped with error, auto spawn a moniter session told to
fix. moniter session will be choosen from usable flash, auto switch if it also
errored".

A monitor is a session the *server* starts, on a model the server chose, with a
report the server wrote. What differs between monitors is what they watch and what
the report says; everything else -- the session, the model search, the retry, the
delivery through `commands::deliver` -- is one implementation.

| kind | session | watches | skill |
| --- | --- | --- | --- |
| error | `@monitor/error` | `sessions.status = 'error'` | `monitor` |
| disk | `@monitor/disk` | a tool failing on a disk with no room left | `clean-disk` |

`Kind` in `monitor.rs` is that table. The disk kind's trigger is the device's own
safety space, and it has its own note: `server-disk.md`.

Three decisions carry the whole of it: what a trigger *is* (data, not an event),
which session it goes to (`@monitor/<kind>`, one per kind), and how that session
is chosen and re-chosen.

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

## One session per kind, under a name nobody else may take

The sessions are `@monitor/error` and `@monitor/disk`. A leading `@` is the
harness's namespace, and `set_session` refuses it -- whole or by segment -- with a
sentence saying so. This is why the reservation is worth having at all: the harness
never has to look at a session and decide whether it is its own, and a person's
session called `monitor` -- which 2026-11-02 showed is a name people do use -- is
simply their session. The slash is the same hierarchy every session name has, so
`@monitor/error` sorts and nests like any other name, and a monitor of our own can
still be a child of another. Deleting one is allowed, because a monitor that cannot
work has to be removable; the next trigger creates it again.

`@` rather than `#` because a `#` that opens a word is a comment to
`shell_words::split`, so `--session=@monitor/error` is what a hand types and nothing
needs quoting. The references a message carries are `@image(...)`,
`@skill(...)`, `@file(...)` -- each an opening parenthesis -- so a bare
`@monitor/error` is not one of them,
and the report a monitor is given never names itself anyway.

The session is created with no `--cwd`, so `SessionManager` gives it the server's
own directory, exactly like a session made from a window.

Delivery is `commands::deliver`, the same path `send_message` takes: idle means a
turn starts now, busy means an inbox row the next step boundary reads. The queue
gate, `@skill(...)` expansion, shells, tools, events, and the window's rendering are
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

A monitor is an ordinary session, so its own failure is an ordinary
`sessions.status = 'error'` row -- `self_failed` is "the name starts with
`@monitor/`", which is now a fact rather than a guess. It goes to
`@monitor/error`, because "why does this session keep failing" is that kind's
question whatever the failing session was.

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

When the list is exhausted the sweep writes one `Event::Log` and stops: the kind's
session has failed on every usable flash model, and the report is left for a
reader. No recursion, no invented retry budget -- `AGENTS.md` forbids one, and the
failure is still visible in the window either way.

The disk kind counts its own attempts, because it has no failing session row to
hang them on: `monitor_state` holds the event it last handed over
(`disk.event`), how many models that handover has been through
(`disk.attempts`), and the row the last report became (`disk.wake`). While that
row's session is in `error`, the same event is handed over again on the next
candidate -- a disk whose first choice of model is refused is still cleaned, and
the error kind is told about the failing monitor in parallel.

## What the report says, and what the skill says

The first version put everything in the message. A measured run then spent 112
seconds and thirty tool calls finding out what a gongga home looks like -- where
the database, the log and the lock are, which table holds the transcript, where
the roster and the keystore live, whether a `400` means the harness built a bad
request -- and that map is the same for every failure.

So the knowledge is split by how long it lives:

| where | what | what it costs |
| --- | --- | --- |
| `@skill(monitor)` (ship), in the prompt's index | the map, the triage table, the read-only schema, the safety rules, how a report ends | charged once, reused by every later request of the session |
| the message | this failure: session, model, row range, build, home, pid, cwd, the `notice` the server wrote, the read-back command | charged once, and it rides the transcript |

The reference is what makes the first row work: a message that names a skill makes
that skill's body a root, `agent::index_of` appends it to the system prompt's index,
and the mention itself stays literal in the message (SPEC's meta prefix cache
optimize). Nothing about the *dynamic* half needs a skill, so a home that installed
none still gets a report worth acting on; it just gets the map by exploring.

Two things are deliberately still in the message: the read-back command (it names
the failing session) and `@skill(self-control)`, because a monitor that decides to
change source and restart the server needs the turn-preserving stop, and that is
the one action it must not improvise.

## Why a skill and not the directory's `AGENTS.md`

`meta::roots` also seeds `@file(AGENTS.md)` from the session's own directory. That
works, and it worked in the failing run: the first prompt's index carried
`AGENTS.md` (21,400 characters). The rows that did not were **later** rows, and
the reason is the one thing the index follows that the row does not show.

`SessionHandle::cwd()` is `ShellSet::reported_cwd()` -- the directory the session's
*shell* is in, which `remote::Shell` refreshes with a `pwd` after every command.
The index is rebuilt from that live value at every step; `prompt.cwd` in the row is
frozen at the first stored value (`prompt_in_force`: "the stored row's answer once
there is one"). So every row claims one directory while the index behind it was
built in whatever directory the last tool call left the shell in.

Measured on the failing run, exactly:

| row | what the shell had just done | index |
| --- | --- | --- |
| 2 | nothing yet | `AGENTS.md` + `self-control`, 21,400 |
| 11 → 13 | `cd /c/Users/17124/.gongga && ls -la` | `self-control` alone, 12,017 |
| 45, 63 | `cd /c/resource/gongga && …` | `AGENTS.md` back, 21,400 |

That is why a monitor turn's map disappeared precisely when it started exploring:
its first useful command was a `cd` into the home.

Fixed on 2026-11-02, when this was traced: `meta::roots` now reads the seeded
`AGENTS.md` in the session's own directory instead of the shell's, so the map stays
put (see `server-agent.md`). The skill is still the right carrier for a monitor's
knowledge, for the reason that outlives the bug: it resolves through
`<home>/skills`, a monitor turn `cd`s constantly, and a report must not depend on
where the last tool call left the shell.

So the reason a skill is the right carrier is not "there is no cwd yet" -- there
is one, and it is right at the first prompt. It is that a skill resolves through
`<home>/skills` and does not move when the shell does, and a monitor turn moves
its shell constantly. It also means the map cannot silently vanish mid-turn.

The selftest asserts the property directly: `@skill(monitor)` must be in the prompt
row's `rooted`, and the skill's own words must be in the index.

The index is not free: 38,190 characters in this repository, because `AGENTS.md`
names `SPEC.md` and `whiting-dev` and the walk is recursive. That is the price of
having the map in a cached prompt instead of a transcript, and it is paid once.

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
| `set_session '@mine'` | refused: the `@` prefix belongs to the harness |
| a session on the refused model | `turn end … outcome=Failed`, `status='error'` |
| the handover | `handed a failure to the monitor session=broken model=dead/flash-dead`, `@monitor/error` created, `monitor_seq` past the transcript |
| the monitor's own turn | `turn end session=@monitor/error outcome=Failed`, then `model=fake/flash-fake attempt=2`, and that turn ends `Done` |
| the retry's transcript | the original report, unchanged, behind a preface naming the failed model |
| six more seconds of ticks | no second report for the same failure |
| a tool failing with a full disk's words | the device's reserve is spent, one `disk_space` event row carries the session, the tool and the volumes, and `@monitor/disk` is created with `@skill(clean-disk)` in its prompt index |
| the disk monitor's first model refused | `turn end session=@monitor/disk outcome=Failed`, then the same report on the next model ends `Done` |
| after that turn | the reserve file is armed again |

The race the selftest cannot see -- two servers spending one reserve -- is
`disk::tests::two_servers_share_one_claim_and_only_one_wins`; see
`server-disk.md`.
