# 2026-11-10 — SPEC's `cancel`, finished: what a stop leaves, and who picks it up

The operator said "按照 SPEC 的精神自助推进" and then the machine ran out of disk
mid-build; when space came back the work continued from the same tree. This note
records what SPEC's `cancel` paragraph now means in code, the gates that were run
(for real, with their exit codes), and the one thing this session did not do.

## SPEC's four sentences, and where each one lands

| SPEC | where |
| --- | --- |
| "queue following notices" | `commands::deliver` — a message that arrives while the server is stopping becomes a pending row instead of a turn |
| "running and not waiting -> canceled flag, restart continues" | `interrupt` (already there) |
| "running and waiting -> waiting flag, restart continues the waiting for the lefting time" | `StopSignal::wait_until` + `sessions.waiting_until` + `resume_pending`'s clock |
| "killing channel / failed to adopt -> channel end notice" | `Channels::kill_all`, `Channels::adopt` |

The wait is stored as *when it ends*, in epoch milliseconds. A duration would be
wrong: the process that reads it is not the one that wrote it, so "six seconds from
now" is a different instant by then.

`cancel` is its only writer. My first version put the write in `interrupt`, the helper
`cancel` calls per session, and that is wrong against SPEC: `interrupt_session` is its
own verb ("end tool waiting, return result as tool call result") and the turn goes on
after it, so a deadline written there would be resumed by a later restart for a turn
that had already ended. `cancel` now reads each running session's deadline, writes it,
and only then interrupts — the order the flag needs, since after the token no turn is
left to ask. `del_session` writes nothing; the row goes with the session.

Three readers have to honour that row or the wait is silently skipped: `deliver`
(as above), `nudge_inboxes`, and the pending-row loop in `resume_pending`. Each skips
a session whose deadline is still in the future. `resume_pending` then arms one timer
per waiting session, sleeps what is left, clears the row and starts the turn.

## Gates, and what they said

`cargo build` finished; `cargo fmt --check` exit 0; `test-layout-check` 0 violations;
`scripts/skill_contract.sh` PASS 8 skills; `scripts/ship.sh --check` "in agreement",
exit 0. `comments-check` went 16 baseline violations -> 14: the long comments this
work first carried were cut to the 120-character preamble and their reasoning moved
into `docs/notes/server-session.md` and `docs/notes/server-channel.md`.

Real processes, not readings:

- `scripts/drain_selftest.sh` PASS · `cancel` interrupts, nothing owed afterwards.
- `scripts/channel_selftest.sh` PASS (26s) — with a fresh home; see the next section.
- `.dev/probe_wait.sh`: a row with `waiting_until = now+6s`, server killed, second
  start logs `continuing the wait a stop cut short waited_ms=2457` and the row is
  `None` afterwards. That is the clock, measured.
- `.dev/probe_adopt.sh`: a channel row whose process is dead is reported gone
  (`channels the last run kept were gone when adopted gone=1`) and its notice becomes
  a pending turn row for the session.

## Two things that were not this change's fault

**`channel_selftest.sh` and `cmd_selftest.sh` leave their homes behind.** `rm -rf` on
`.dev/selftest/<name>-home` is intercepted by the environment's safe-delete shim,
which fails with `[safe-delete][SAFE_DELETE_FAIL_CLOSED] ... trash-failed`, so the
next run starts on the previous run's database. The visible symptom is
`session "copy" already exists` and two FAILs that have nothing to do with channels.
Running the same assertions against a per-pid home passes. Copies used this session:
`.dev/channel_selftest_fresh.sh`, `.dev/cmd_selftest_fresh.sh`.

**`cmd_selftest.sh` has 5 FAILs on `fc44148` too.** `git stash push` of this work,
`cargo build`, the same script: identical five (`bash main <command>`, the persistent
shell, `read_file`, and both `delete_session` checks). They are the forwarding
migration's open end (Q5 in `2026-11-08-spec-cmd-refactor-wip.md`), not a regression.
The changes were restored from the stash afterwards.

## The stash race, and what it cost

`git stash push`/`pop` are repository-wide, and this checkout shares `.git` with
another session working on `feature/client-split`. Between the push and the pop the
stash stack was rewritten under this session, and the pop applied the *other* tree's
baseline: `window_index.rs` delete/modify, `render.rs` losing `panes.begin_frame()`,
conflict markers in three `tui` files. Recovery, in order: the work was saved as
`.dev/cancel-wait.patch` (`git diff fc44148 3cd46a7`), the tree was reset to
`fc44148`, and the patch applied with `--3way` — all seven files clean. Lesson for the
next agent: in this repository a stash is not private. Prefer a patch file for
anything that must survive a build.

## Still owed

- Q1/Q2 in the 2026-11-08 note (`session`/`id` argument spellings, `keep_alive`):
  still unanswered, still a cross-cutting CLI rename.
- Q6, the `## Python Cmd` section, is untouched.

## Afterwards: SPEC dropped the flag's name, and the code followed

The operator removed the line `# for sessions idle or calling \`wait\`, do not set
interrupted flag.` from `interrupt_session` in SPEC. What is left is one sentence in
`cancel`: "for running sessions that are not waiting, set canceled flag to them,
restart will auto continue session with cancel flag". So the flag is `cancel`'s, it is
called `canceled`, and no other verb writes it.

Code changes, in one pass: `sessions.interrupted` is renamed to `sessions.canceled`
(`ALTER TABLE ... RENAME COLUMN`, the idiom `migrate` already uses for `llm` ->
`provider`, so an old home keeps its rows); `set_interrupted` + `set_waiting_until`
become one `record_stop(id, canceled, until)`, because the two flags are exclusive and
one write is how a row cannot say both; `interrupt` no longer writes anything -- it
fires the token, and its `keep` argument is only about whether the turn's queued rows
outlive it (cancel and `interrupt_session` keep, `del_session` drops).

Measured, two arms, a turn that streams for seven seconds:

- `cancel` mid-answer: `canceled=1`, `waiting_until=NULL`; after the next start both
  are `0`/`NULL` again and the turn was resumed.
- `interrupt_session` mid-answer: `canceled=0`, `waiting_until=NULL`, and the turn is
  not resumed by the next start -- it ended with its words in context.

A start also no longer leaves an `interrupted` column behind: the `add_column` list
still carried the old name, which is how the first run of the probe showed both
columns in a fresh database.
