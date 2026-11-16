# `ask` does not wait (2026-11-16)

> **Superseded, same day.** The channel and the `answer.py` waiter this note describes are
gone: `subscribe` now takes a list of commands and puts a matched event's fields into their
string arguments, so `ask` arms one `send_message` carrying `{event.answer}` and nothing
waits in a channel. The two findings below still stand -- a blocking call is ended by the
message it waits for, and a Windows shell rewrites a word that starts with `/` -- because
they are about the harness, not about this design. See `docs/notes/server-subscriptions.md`.

## The observation

Session `Rusd` called `gongga_cli{run_skill{ask}}` and the tool call held the turn for 97 s.
It ended with `` `run_skill` was cut short by the harness `` -- and the thing that cut it was
the answer it was waiting for:

| moment | what |
| --- | --- |
| `1794838307259` | Rusd's `gongga_cli` call starts (`tool_calls.started_at`) |
| `1794838404145` | the asked session's `send_message` back to Rusd |
| `1794838404269` | the tool answer: cut short, 97.0 s |
| `1794838404307` | the same message, as an inject row in Rusd |

## Why it could wait at all

Two mechanisms, neither of them `exec`:

1. **Every tool call is a wait.** `tools/mod.rs::invoke` -> `waited()` (`:816`): it polls
   `ctx.inbox` every `INBOX_POLL` 250 ms, and on any message cancels the call's token
   (`:852`), gives it `EARLY_RETURN` 1 s, then aborts. So "a waiting tool" is the
   dispatcher's property, not `exec`'s -- SPEC gives waiting to `exec`'s channel and to the
   `wait` tool.
2. **The length came from the skill, not the harness.** `ask_session.py` polled
   `get_messages` with `time.sleep(1)` for up to 900 s, and `run_skill` is exempt from the
   command clock (`commands.rs::OWN_CLOCK`), so the only bound was the skill runner's
   `CALL_TIMEOUT_MS` (120 s) -- which the declared 900 s could never fit inside.

The two facts contradict each other: the message that says the answer exists *is* the
message that cancels the waiting call, so a blocking `ask` can never be the one that
receives it.

## Verdict (user, 2026-11-16)

> B. ask = send message 加自动subscribe
>
> 不是返回，是 ask 自动创建一个 channel，订阅，回调用 channel 跑一个脚本，get message，
> send message
>
> 顺便加上一个 new_channel_id() 的 server cmd 来防止重名

## What it is now

`ask(session, question)` does four things and returns:

1. `get_messages -1:-1` for the cursor -- the answer is at or after `cursor + 1`;
2. `new_channel_id(prefix="/ask")` -- the server mints `<prefix>/<n>`, the smallest free n;
3. `new_channel` with that id, program `bash`;
4. `subscribe(patterrn="^turn_end:<mentor>$", cmd=input_channel{id, input, wait:0})`, then
   `send_message` the question.

At the mentor's turn end the callback types one line into the channel:
`GONGGA_ASK_MENTOR=… GONGGA_ASK_FIRST=… GONGGA_ASK_CALLER=… GONGGA_ASK_CHANNEL=… python answer.py`.
`answer.py` runs **in the channel** while no turn runs: it reads the mentor's transcript from
row `first`, picks the last `output` row with `done` true, `send_message`s it to the asker,
and then ends its own channel.

Nothing blocks: not `ask`, not the turn that asked it. The wait is a program's, which is
what SPEC's `exec`-only waiting allows.

### Two traps this cost

- **A leading `/` is rewritten by the shell.** The first shape passed the channel id as an
  argument: `/ask/9` reached `answer.py` as `C:/SoftWare/git/Git/ask/9` (MSYS path
  conversion), so the waiter deleted nothing. An assignment value is converted too -- the
  second shape carried it in the environment and failed the same way. It travels as
  `ask/9` now, and `answer.py` re-attaches the slash.
- **The waiter ends the channel it is running in**, so `del_channel`'s answer never
  arrives; the call is best effort and reports a channel left behind on stderr, which the
  channel's output keeps.

## Round two (same day, three follow-ups from the user)

1. **The callback's channel deletes itself** -- `answer.py` ends the channel it runs in after
   delivering. Verified: after each answer `ls_channel` is empty.
2. **`del_channel /ask` would not take `/ask/1`.** A subagent (`fix/del-channel`) found it:
   the branch was chosen by *how many* matched (`doomed.len() > 1`) instead of by *whether the
   match is the path itself*, so one channel below a path fell through to `stop(&id)` with the
   path. Fix: `doomed.iter().any(|path| path != &id)`, plus the loop now reports the channels
   that refused to end instead of dropping them. Verified: `ended 1 channel(s) under "/ask":
   /ask/1`.
3. **`ask` refused a session whose workspace is on a peer machine.** `new_channel` had no
   `remote`, so the channel always followed the session. It has one now -- SPEC's
   `optional[str]`, absent is the session's machine and `none` is this one, the same reading
   `exec`'s `remote` gets -- and `ask` states `none`, because the waiter beside the skill is
   here. Verified from a caller whose workspace is `lichen-vm`: `ask` returned in 0.43 s, the
   channel row read `"remote": null`, and the answer arrived.

## Round three

1. **`scripts/skill_contract.sh` could not fail.** It built its file list with `find`, which
   on this machine is `C:\Windows\system32\find.exe`; its refusal went to `/dev/null` and the
   empty list printed `PASS 0 skill(s)` with exit 0. It now globs (a shell's own expansion,
   no external tool) and refuses to pass when it checked nothing. Bare shell: `PASS 10`, and
   a name that matches nothing: `FAIL 0 skill(s) ... this check read nothing`, exit 1.
3. **`del_channel`'s `Ok(false)` arm was dead.** `Channel::stop` answered `Result<bool, String>`
   and never returned false -- a channel whose program had already exited is still a row to
   forget. It is `Result<(), String>` now, and both call sites say so.

## A convention, not a check (user, 2026-11-16)

> 不阻塞是行为约定，无法强制

So `run_skill` stays out of the command clock, and the convention is written where a skill's
author reads it: `ship/skills/skill/SKILL.md`, "A skill must not block", with the measured cost
and the shape that avoids it. `OWN_CLOCK`'s comment says the same thing in the code: exempt
because a skill is a program with its own bound, not because blocking is allowed.

## What changed

- **server** `crates/server/src/commands.rs`: `new_channel_id` (catalog, dispatch, handler),
  and `first_free` shared with `free_subscription_name` so both families of names are
  minted one way. SPEC `## Server Cmd` gains the signature.
- **content** (`97f1fd8`, `823434c`, `2c74464`, `18b35bf`): `skills/self-control/ask_session.py` (the four steps),
  `answer.py` (the waiter), `SKILL.md` (the `ask` section, a `new_channel_id` section, the
  command list), `__init__.py`, `_client.py` (the fallback import directory moved to
  `<home>/ship/python`, where protocol 12 put it), and `skills/skill/SKILL.md` (the no-blocking
  convention).
- `skills/handoff/{SKILL.md,__init__.py}`: the two places that still promised `ask` waits
  and prints the answer.

## Verified (real server, real process)

- `new_channel_id prefix=/ask` -> `{"id": "/ask/1", "session": "fix"}`.
- `ask` returns in **0.325 s**: `` channel /ask/1 waits ... (subscription `sub1`) ... ``.
- The probe's turn ends; `events kind=subscription` shows
  `input_channel … ok=true`; the message arrives: `fix/probe answered (asked through
  self-control `ask`): 我现在的会话名是 fix/probe。`
- The waiter deleted its own channel: `ls_channel` empty afterwards.
- Gates: `PATH=/usr/bin:$PATH bash scripts/skill_contract.sh` -> `PASS 10 skill(s)`;
  `cargo build -p gongga-server` clean; `cargo fmt --all`; `ship.sh` exit 0;
  `ship.sh --check` exit 0 (protocol 13). One evolve (`20261116-224414`) carried the new
  command into the running server.

## Left open

- Nothing of this round's; the decisions it rested on are recorded above.
