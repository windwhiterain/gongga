# The capped-output notice names a program the relay can start

2026-11-22, session `mask-offload` on the live home. Every `sed` and `rg` call died with
`the relay ended: gongga-relay: starting "sed" in "C:\resource\gongga": program not found`,
while `git` and `bash -lc` worked, and the turn was cancelled mid-recovery.

## Measured, not inferred

A one-off probe ran `Command::new(name)` under the exact PATH the failed channel's `cmd`
file carries (from `~/.gongga/channels/ch-70a44…/cmd`):

```
sed,  own process PATH        REFUSED: program not found
sed,  channel PATH (relay's)  REFUSED: program not found
rg,   channel PATH            REFUSED: program not found
grep, channel PATH            REFUSED: program not found
git,  channel PATH            STARTED (exit=0)
bash, channel PATH            STARTED (exit=0)
sed,  channel PATH + Git\usr\bin  STARTED (exit=0)
```

`program not found` is Rust std's own wording when its PATH search misses — byte-identical
to the relay's last words, so the relay did exactly its job. `sed.exe` exists only at
`C:\SoftWare\git\Git\usr\bin\sed.exe`, which Git's installer deliberately keeps off the
Windows PATH (`Git\bin` holds only bash/git/sh); `rg` is not installed at all. The relay
resolves bare names against that PATH (`relay.rs::program_of` special-cases only
`bash` and `python`).

## Why it was not purely the model's fault

The content's own `output_cap` notice recommended `sed -n 'START,ENDp' {{path}}` or
`grep -n PATTERN {{path}}` — and `grep` fails the same way. A model following the harness's
own advice hits the wall; only `rg` was the model's own habit. The notice's `{{path}}` is
already spelled in Git Bash dialect (`spill.rs::shell_path`), so wrapping the same commands
in `bash -lc` works on every machine — the form the server itself uses
(`remote/pick.rs` builds `["bash", "-lc", script]`).

## Fixed in content

`ship/config/config.toml`, `output_cap` now says
`bash -lc "sed -n 'START,ENDp' {{path}}"` / `bash -lc "grep -n PATTERN {{path}}"`,
published as content commit `73a3925` on `main` (`scripts/ship.sh -m`, then `--check`
exit 0). A running server installs it at its next start — the live one does not have it
until restarted.
