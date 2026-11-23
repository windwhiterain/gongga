# Peer mask-capable endpoint runs as a systemd user service

2026-11-23 user direction: keep the mask-capable endpoint resident ("把 serve 常驻，我要用").
The peer serve was stopped after the `1b66fe7b` / `53d7eb64` verification, and the roster's
stop instruction (`peer ~/kv-mask/exp/stop_minicpm.sh`) is no longer the way to stop it.

## What now owns the endpoint

`whting@192.168.1.3`, systemd user unit `minicpm-endpoint.service`
(`~/.config/systemd/user/minicpm-endpoint.service`), enabled and active, linger on
(`loginctl enable-linger`), so it starts at boot and survives logout.

- `Type=forking` + `PIDFile=~/kv-mask/exp/minicpm.pid`, `ExecStart=` the existing
  `~/kv-mask/exp/start_minicpm_fixed.sh`, so the flags still live in exactly one place.
- `Restart=on-failure`, `RestartSec=10`, `SuccessExitStatus=143`: a crash restarts, a
  `systemctl --user stop` does not.

## Stop / inspect

```
systemctl --user stop|start|restart minicpm-endpoint.service
systemctl --user disable minicpm-endpoint.service   # remove it from boot
systemctl --user status minicpm-endpoint.service
```

`~/kv-mask/exp/stop_minicpm.sh` now only kills the serving pid: systemd then treats that as a
failed run and restarts it 10 s later. Use the unit, not the script.

## Verification (2026-11-23, service start)

- `systemctl --user is-enabled` = `enabled`, `is-active` = `active`, main pid inside
  `.../minicpm-endpoint.service` cgroup.
- `/v1/models` answered 21 s after `systemctl --user start`:
  `minicpm5-2b max_model_len=131072 max_unmasked_model_len=55296`, matching the roster row
  `minicpm5-2b` / route `kvmask-peer` in `~/.agents/gongga-agents/providers.toml`.
- From this machine over the LAN, `POST /v1/chat/completions` finished with
  `finish_reason=stop` and 27 completion tokens (thinking on).
- `--api-key sk-fake`, so a request without the bearer header answers `Unauthorized`; the key
  is `~/.agents/keys.toml [keys] kvmask-peer`.

The long-tape masked prefill timeout recorded in `.dev/verify/1b66fe7b1beb-unmasked-context.json`
is unchanged: this note is about the service, not about that measurement.
