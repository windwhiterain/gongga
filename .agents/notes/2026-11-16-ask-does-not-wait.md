# `ask` does not wait (2026-11-16)

## The observation

Session `Rusd` called `gongga_cli{run_skill{ask}}` and the tool call held the turn for 97 s.
It ended with `` `run_skill` was cut short by the harness `` — and the thing that cut it was
the answer it was waiting for:

| moment | what |
| --- | --- |
| `1794838307259` | Rusd's `gongga_cli` call starts (`tool_calls.started_at`) |
| `1794838404145` | the asked session's `send_message` back to Rusd |
| `1794838404269` | the tool answer: cut short, 97.0 s |
| `1794838404307` | the same message, as an inject row in Rusd |

## Why it could wait at all

Two mechanisms, neither of them `exec`:

1. **Every tool call is a wait.** `tools/mod.rs::invoke` → `waited()` (`:816`): it polls
   `ctx.inbox` every `INBOX_POLL` 250 ms, and on any message cancels the call's token
   (`:852`), gives it `EARLY_RETURN` 1 s, then aborts. So "a waiting tool" is the
   dispatcher's property, not `exec`'s — SPEC gives waiting to `exec`'s channel and to
   the `wait` tool.
2. **The length came from the skill, not the harness.** `ask_session.py` polled
   `get_messages` with `time.sleep(1)` for up to 900 s, and `run_skill` is exempt from the
   command clock (`commands.rs::OWN_CLOCK`), so the only bound was the skill runner's
   `CALL_TIMEOUT_MS` (120 s) — which the declared 900 s could never fit inside.

The two facts contradict each other: the ping that says the answer exists *is* the message
that cancels the waiting call, so a blocking `ask` can never be the one that receives it.

## Verdict (user, 2026-11-16)

> B. ask = send message 加自动subscribe

`ask` is `send_message` plus an automatically armed subscription. It does not wait.

## What changed (content `9932985`, `e335b07`, `88437c2`)

- `skills/self-control/ask_session.py`: `ask` = read the cursor → arm
  `^turn_end:<session>$` with a `send_message` ping that names row `since + 1` → send the
  question → return one sentence (the subscription's name and the range to read).
  `timeout` / `poll` are gone with `_wait` / `_read` / `_answer_in`; asking yourself is
  refused; a question that never went out disarms its own ping.
- `skills/self-control/_client.py`: the fallback import directory is `<home>/ship/python`
  (protocol 12 moved the Python surface out of a home's `lib`).
- `skills/self-control/SKILL.md`: the `ask` section, the frontmatter and the client path.
- `skills/handoff/{SKILL.md,__init__.py}`: the two places that still promised `ask` waits and
  prints the answer — the continuation's hint and the prose beside it.

## Verified (real server, real process)

- refusal path: exit 1, `name the session to ask`, nothing armed.
- end to end against a throwaway session: `ask` returned in **0.175 s** with
  `` subscription `sub1` ... from row 1 ``; that session's turn ended (`seq 5`, idle); the
  ping arrived as a user message naming row 1; the subscription was consumed
  (`ls_subscription` empty — one match, not persistant); the answer sits in rows 1..5.

## Left open

- **A was not done**: `run_skill` is still in `OWN_CLOCK`, so a skill that blocks still
  holds a turn up to `CALL_TIMEOUT_MS`. `ask` no longer does.
- The running server reads the content it pulled: the home's `<home>/ship` holds the old
  `ask` until that checkout's next pull (hourly, or a start).
- `scripts/skill_contract.sh` prints `PASS 0 skill(s)` when `find` is Windows' `FIND` —
  which is the case in every shell this server starts (no Git `usr/bin` on `PATH`): the
  `2>/dev/null` hides `FIND: Parameter format not correct`, so the gate cannot fail. With
  `PATH=/usr/bin:$PATH` it checks 10 and passes.
