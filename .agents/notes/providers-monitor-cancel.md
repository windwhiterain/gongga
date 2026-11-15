# The providers monitor must not restart the server

2026-11-15. The `@monitor/providers` session ran `cancel` on the live home three times in
one day and interrupted every turn that was running. It believed it was reloading the
roster. Nothing on this machine needs that reload.

## What happened (13:06:59Z, home `~/.gongga`)

- `events` kind `server_stop`, id 362415:
  `{"by":"python at 127.0.0.1:55123, from session @monitor/providers","pending":4,"sessions":4}`.
- Four turns ended `cancelled`: `fix` 1371.0 s (~278 steps), `error` 434.9 s (31 steps),
  `GUI` 317.8 s (26 steps), and the monitor's own 1274.3 s.
- The window then showed nothing: 7.6 s later the resumed `GUI` and `error` turns failed in
  9 ms with `no provider "deepseek-official": the roster has no providers`, and the server
  stayed down until 13:30:38 (23.5 min) when the bundle started it again.
- Same actor at 06:10:48 and 07:03:13; `a tool in session fix` at 12:39:13 was the same
  shape from the other side.

## Why it did it

The monitor prompt said it should:

    If it is a harness bug, `@skill(self-control)` says how to restart the server without
    losing a turn.

`self-control`'s only restart is `cancel`, and `cancel` is the machine's stop -- it
interrupts every running turn. "Without losing a turn" was never true. The report it was
answering was `lichen-vm/gaps` overflowing a 1048576-token window, i.e. a harness bug.

## The fact that makes the sentence unnecessary

`providers.rs` `Handle` is "a live view of the roster: every question asks the file first",
and `roster()` re-reads on a stamp change with `keys.toml` in the same watched list. So an
edit to the roster or the keystore takes effect on the session's next step, and a
`generation` bump retires every cached client. No restart is part of any provider fix.

## What changed

Content only, commit `2fa2d45` in `gongga-ship` (protocol 13 unchanged):

- `config/config.toml` `monitor_error`: the restart clause is replaced by the live-read
  fact, and a harness bug is "say so and stop".
- `skills/providers/SKILL.md` section 3: "改完就生效，不用重启", with `cancel` named as the
  machine's stop rather than a reload.

Gates: `scripts/skill_contract.sh` PASS, `scripts/ship.sh --check` exit 0. The prompt is a
`OnceLock` read at startup, so the running server still has the old words until it starts
again -- deliberately not restarted, since restarting is the thing that was wrong.

## Open

A hard guard was proposed and not taken: make `cancel` refuse (or require `--force`) while
another session has a running turn, so a misjudging agent cannot take 278 steps of someone
else's work with it.
