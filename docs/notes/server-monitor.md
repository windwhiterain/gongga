# server-monitor: the sessions the harness hands a report to

SPEC: "providers: spawn/notice when a session failed on an llm request (refused, or
the answer never finished) ... the session is handed the `providers` skill"; "disk:
spawn/notice when free space is under certain threshold".

A monitor is a session the *server* starts, with a report the server wrote -- and
nothing else about it is special: it is created like any session, it runs on the
model its own row names, and it is configured with `set_session`, the same command a
person uses. What differs between monitors is what they watch and what the report
says; everything else -- the session, the delivery through `commands::deliver` -- is
one implementation.

An earlier draft also had a monitor "choosen from usable flash, auto switch if it
also errored". That is gone with the machinery it needed: a model the harness picks
is a model its operator cannot change, which is the first thing anybody wants to
change about a monitor that keeps failing.

| kind | session | watches | skill |
| --- | --- | --- | --- |
| providers | `@monitor/providers` | a turn whose `turn_end` says `origin: provider` | `providers` |
| disk | `@monitor/disk` | the room left, read after every tool call | `clean-disk` |

The trigger is narrower than "a session is in error", and deliberately so: SPEC's
monitor watches this machine's **providers**. A turn that failed because the model
refused the request, never finished its stream, or could not be resolved at all
writes `origin: provider` into its `turn_end` row, and only that origin is handed
over. A session that broke anywhere else stays in `error` and is nobody's report --
which is why the origin travels on the failure rather than being read back out of
the wording of a log line.

`Kind` in `monitor.rs` is that table. The disk kind's trigger is a free space
reading compared against a threshold, and it has its own note: `server-disk.md`.

Three decisions carry the whole of it: what a trigger *is* (data, not an event),
which session it goes to (`@monitor/<kind>`, one per kind), and that the session is
an ordinary one.

## A failure is a row, not an event

The trigger is `sessions.status = 'error'`, read from the database:

```
status = 'error' AND monitor_seq < the session's newest message seq
```

A stop is not a failure. `interrupt_session` fires the request's cancel token, and every
cancel arm in `llm.rs` ends the step `Stopped`: the turn is `Cancelled`, the row's status
is `Idle`, and nothing here is woken. The arm that waits for response headers used to send
its cancel as `LlmEvent::Failed` instead, which put a pressed stop in `error` and handed
the monitor a report about a provider that had not been given the chance to answer.
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

## One session per kind, and every name a person's

The sessions are `@monitor/providers` and `@monitor/disk`. Those are names this
module wrote down, and **no name is reserved**: `set_session` accepts whatever
SPEC's grammar accepts, `@` and every segment after it included, and the harness
never asks a name whether it is its own -- it knows its sessions by the constant it
created them under.

The cost is stated rather than hidden: a person who names a session `@monitor/disk`
has a session the next disk report is delivered to. That is the same bargain as a
person naming one `main`; the harness's handle on a name is a constant in one file,
and the namespace itself belongs to the operator.

`@` rather than `#` because a `#` that opens a word is a comment to
`shell_words::split`, so `--session=@monitor/disk` is what a hand types and nothing
needs quoting. The references a message carries are `@image(...)`, `@skill(...)` and
`@file(...)` -- prefixes with an opening parenthesis -- so a bare
`@monitor/providers` is never read as one (`meta::mentions`).

Creation is `commands::create_session`: `set_session` with a name and, when the caller
has one, a model. A monitor comes into being exactly where a session a person named
would -- the server's own directory as its cwd, the default tuning -- and its model
is drawn once, immediately afterwards, by `model_pool` (see the section below). An
existing row is never written here.

Its configuration is the ordinary command, and it is the whole of it:

    gongga-cli '{"name":"set_session","args":{"id":"@monitor/disk","model":"openai/gpt-5","cwd":"C:/work"}}'

Nothing re-tunes the session on a later handover, so the model, the tier, the cwd,
the keep-alive and the compaction settings are the operator's and stay. The report
says which model its own turn runs on (`{{mine}}`), read from the row at handover
rather than from a choice made for it.

Delivery is `commands::deliver`, the same path `send_message` takes: idle means a
turn starts now, busy means an inbox row the next step boundary reads. The queue
gate, `@skill(...)` expansion, shells, tools, events, and the window's rendering are
all reused; there is no second way to start a turn.

## The model is drawn once, from `model_pool`

The model a monitor runs on is not chosen by this server. At creation `monitor::draw_model`
asks the `model_pool` skill for one -- `pick(tier=...)`, the same call a spawn makes,
through the same door `run_skill` uses: the package's contract, its call on stdin,
`GONGGA_SKILL_LIB`. The tier is a field of `Kind`, so a monitor's work decides it:
both kinds state `flash`, because reading a roster or cleaning a disk is not a
judgement call.

What the draw buys is the pool's own gates -- the whitelist in `preference.toml`, the
credential this machine actually has, the allowance above its floor. What it costs is
that the server must not keep a second opinion: there is no flash search, no ranking
by allowance and no `Catalog::speaks_responses` gate here, because re-deciding what
`pick` decided is how the two come to disagree.

A draw that cannot answer -- no pool on this machine, nothing left in the tier, a wall
it cannot see past, a name `set_session` then refuses -- leaves the session on
`DEFAULT_SESSION_MODEL` and logs why (`no model drawn: ...`). The monitor still gets
its report: one that goes silent because it could not choose is the worse failure.

Nothing re-draws it. Not a later handover, not a failure of the monitor's own model:
what stands in for a switch is a session in `error`, drawn by the window like any
other, and one `set_session` away from running on something else. `quota::of` is not
in this path; the `active_quota` block still reads `quota::readings()`.

## When the monitor itself fails

A monitor is an ordinary session, so its failure is an ordinary
`sessions.status = 'error'` row. What keeps that from looping is identity rather
than a name pattern: `sweep_errors` skips the failure of the session a report would
be delivered to (`failure.name == PROVIDERS.session`), because handing it another
report would be the same report forever. It stays in `error` for a reader.

A failure of `@monitor/disk` is a different session's, and goes to
`@monitor/providers` like anybody else's: a provider is what broke, whatever that
session was doing.

The disk and quota kinds keep one cursor each in `monitor_state` -- `disk.event`,
`quota.event` -- and nothing else, because the event row is the whole memory of what
has been handed over. A handover that cannot happen at all (no session could be
created, the report could not be written) advances the cursor and publishes one
`Event::Log`; one the session would not take (`Refused`: it is being deleted, or the
server is stopping) leaves the cursor where it is and the next tick asks again.

## What the report says, and what the skill says

The first version put everything in the message. A measured run then spent 112
seconds and thirty tool calls finding out what a gongga home looks like -- where
the database, the log and the lock are, which table holds the transcript, where
the roster and the keystore live, whether a `400` means the harness built a bad
request -- and that map is the same for every failure.

So the knowledge is split by how long it lives:

| where | what | what it costs |
| --- | --- | --- |
| `@skill:monitor` (ship), in the prompt's plan | the map, the triage table, the read-only schema, the safety rules, how a report ends | charged once, reused by every later request of the session |
| the message | this failure: session, model, row range, build, home, pid, cwd, the `notice` the server wrote, the read-back command | charged once, and it rides the transcript |

The reference is what makes the first row work: a message that names a skill makes
that skill's body a root, `agent::plan_of` appends it to the system prompt's plan,
and the mention itself stays literal in the message (SPEC's meta prefix cache
optimize). Nothing about the *dynamic* half needs a skill, so a home that installed
none still gets a report worth acting on; it just gets the map by exploring.

Two things are deliberately still in the message: the read-back command (it names
the failing session) and `@skill: self-control`, because a monitor that decides to
change source and restart the server needs the turn-preserving stop, and that is
the one action it must not improvise.

## Why a skill and not the directory's `AGENTS.md`

`meta::roots` also seeds `@file:AGENTS.md` from the session's own directory. That
works, and it worked in the failing run: the first prompt's plan carried
`AGENTS.md` (21,400 characters). The rows that did not were **later** rows, and
the reason is the one thing the plan follows that the row does not show.

`SessionHandle::cwd()` is `ShellSet::reported_cwd()` -- the directory the session's
*shell* is in, which `remote::Shell` refreshes with a `pwd` after every command.
The plan is rebuilt from that live value at every step; `prompt.cwd` in the row is
frozen at the first stored value (`prompt_in_force`: "the stored row's answer once
there is one"). So every row claims one directory while the plan behind it was
built in whatever directory the last tool call left the shell in.

Measured on the failing run, exactly:

| row | what the shell had just done | plan |
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

The selftest asserts the property directly: `@skill:monitor` must be in the prompt
row's `rooted`, and the skill's own words must be in the plan.

The plan is not free: 38,190 characters in this repository, because `AGENTS.md`
names `SPEC.md` and `whiting-dev` and the walk is recursive. That is the price of
having the map in a cached prompt instead of a transcript, and it is paid once.

## The boundary with `model_pool`

The server chooses no model for a monitor, so it reads no `tier` and no allowance to
do it. `preference.toml` weights, `model_pool mark`, the entitlement probe and the
choice of a model for a session stay in the skill and in `set_session`, because they
are this machine's policy and not the harness's. Running the skill's python from the
server would also leave a wrong answer on a machine where the skill is not
installed, which is the "two implementations" this avoids by not starting.

## Verified

`scripts/monitor_selftest.sh`: scratch home, ports the kernel picks, a fake
Responses endpoint that refuses one model id (`FAKE_LLM_FAIL_MODEL`), and a roster
of two flash models where the first is the refused one:

| step | result |
| --- | --- |
| `set_session '@mine'` | accepted: no name is reserved, and the session exists afterwards |
| a session on the refused model | `turn end ... outcome=Failed`, `status='error'` |
| the handover | `handed a failure to the monitor session=broken`, `@monitor/providers` created on the model `set_session` gives a session naming none, `monitor_seq` past the transcript |
| `set_session` on the monitor | accepted, and the row carries the model it named -- and it is still there after the next handover |
| the monitor's own failure | no second report: no handover names `@monitor/providers` as the failing session |
| six more seconds of ticks | no second report for the same failure |
| the disk kind | `bash scripts/disk_selftest.sh`: a successful tool call on a volume below the declared threshold writes one `disk_space` event and creates `@monitor/disk` with `@skill(clean-disk)` in its prompt plan; a second call adds nothing |
| the disk kind's arithmetic | `disk::tests::*`: an ordinary disk says nothing, a low one says it once, a recovered one says it again |

The disk kind is a separate script because it needs no error to have happened
first; see `server-disk.md`.
