# `cancel --keep_alive` keeps channels; the next window's cancel ends them

Reported: "cancel keep_alive 没有生效，cancel 后重连，上一轮的 channel 都被结束了".

## What the code does

`commands::cancel` (`crates/server/src/commands.rs`, ~3089) skips `Channels::stop_every`
when `keep_alive` is true and releases every `Ends::OnCancel` job (`job::release_all`,
reached through `detached::release_detached`). A channel's relay is armed with
`job::arm` (`tools/channel.rs`, `start_here`) precisely so that release lets it go.
`Channels::adopt` then only *reads* every row and reaps the ones whose `status` already
carries an exit; a live relay is silent.

The client's own `cancel` never asks `keep_alive` (`Client::cancel`, `client-lib.md`,
"Client::cancel -- SPEC's default"): a window leaving ends every channel in the home.

## Measured

- Scratch home (`.dev/ka`, current `target/debug`): `new_channel` a `bash`, feed it
  `sleep 600`, then `cancel {keep_alive: true}` -> the server exits, the relay (pid 9696)
  and its program (pid 30820) are still there; a start on the same home lists the row and
  logs no "were gone when adopted". So the protocol half works.
- The user's home, `~/.gongga`, 2026-11-13 (UTC):
  - 14:32:39 `server_stop` `kept_channels=true, channels=0` -- and programs started
    before it kept running long after the server was gone (`ch-aff91654` until 14:36:34,
    `ch-6f61ab06` until 14:35:56), so they really were kept.
  - 14:34:20 next start: `gone=6`, all of them `exit 0` -- one-shot programs that had
    finished on their own. The notice reads "its program had already ended ... nothing
    was left to keep alive", which *sounds* like keep_alive failing and is not.
  - 14:37:42 a second `server_stop`, `kept_channels=false, channels=1` -- a window's
    plain `cancel` ended the one channel still live.

## The open question (asked, not decided)

Should a `cancel` that a *window leaving or taking the home over* sends spare the channels
an explicit `cancel --keep_alive true` kept? Today it does not: `keep_alive` survives the
stop it was asked for and nothing else. Recording the kept state on the row (`channels`
has no such column) and having the kill-all path release instead of end those rows would
make SPEC's "make every channel detached" a state rather than one moment.

## Confirmed by experiment (2026-11-13)

`.dev/ka/keep_alive_experiment.sh` (scratch, one home, two server lifetimes, current
`target/debug`):

- two idle `bash` channels, one answers `echo MARK-BEFORE`;
- `cancel {keep_alive: true}` -> "every channel was kept alive, and the next start adopts
  them"; the server exits and **both programs and both relays stay alive**;
- the next start on the same home: 0 "were gone when adopted" notices, rows `kept` and
  `other` listed, and `echo MARK-AFTER` into `kept` answers `MARK-AFTER`;
- `cancel {keep_alive: false}` right after: "2 channel(s) ended with this server", and both
  programs and relays are gone.

So the flag does what SPEC says. What it does *not* do is survive the next cancel: the
window that connects afterwards sends one with no `keep_alive` (`Client::cancel`) and that
ends them -- measured in the user's home at 14:37:42 (`kept_channels=false, channels=1`).
Measurement: `.dev/verify/f77b4c8dd21f-dirty-keep-alive.json`.
