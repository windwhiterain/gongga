# Step 8: two clients, one server (L3)

User authorised the new script on 2026-11-09 ("授权，开工"). The old DESIGN
line "新脚本需用户许可" came from the shape-1 step list, not from AGENTS.md;
it was a conservative note, not a repo rule. It is now marked done.

## What landed

`scripts/multi_client_selftest.sh` starts one unowned server and three real
`gongga-client` processes:

- A and B are `--client_only`, each with its own `--home`, `--client-home` and
  `--parent`, both dialling the same address and explicit key.
- A third one points at `192.0.2.1` without `--client_only`, to prove a remote
  address neither stops nor spawns a server.
- A one-shot `gongga-cli` shim is the third observer: the sessions A and B
  create are both readable from it.

27 checks: same server pid / `ours=false`, shared sessions, isolated
`client.toml`, A's clean exit without cancelling the shared server, B still
writing afterwards, no per-client `server.lock`, remote no-op, and every stdout
frame valid JSON. Two consecutive runs: `PASS · 17s · multi-client · 8d60e90`,
no manual cleanup in between.

Docs: `DESIGN.md` §1 table, step list and a new "第八步" section; new
`docs/notes/client-multi-client.md` plus its README row.

## Traps found while writing it

1. The cli shim is a `#!/bin/sh` file written by the server, not an `.exe`.
   Probing it with `-x` silently swapped the path for a non-existent
   `gongga-cli.exe`. The Windows-native Python driver runs it via `bash`.
2. Paths passed to that driver must be `cygpath -m` (`C:/...`); MSYS `/c/...`
   paths are invisible to Windows-native Python. The first run failed on the
   first check because of this, with the driver never reaching the children.
3. The bash call originally passed eight arguments while the driver read
   `sys.argv[1:8]` (seven). Fixed by dropping the unused `root`.
4. The server watchdog restarts a killed child in 200 ms. Killing only the
   `server.lock` pid leaves a live server holding the home, so the next run
   cannot remove its directory. The log line `supervisor=<pid>` names the
   watchdog; `stop_servers()` kills that tree first, at both ends of a run.
   This is also why the older `client_state_selftest.sh` occasionally leaves a
   tree behind.

A long PowerShell process-listing command was denied by the sandbox once; the
stale `multi-home` tree was then cleared with plain `taskkill /PID ... /T /F`
using only pids from that home's lock and that run's log. No image-name sweep.

## Gates

`bash -n` on every `*_selftest.sh`, `comments-check` (0), `test-layout-check` (0),
`git diff --check`, `skill_contract` (PASS 8), `cargo fmt --all -- --check`,
`cargo build`, `cargo test -p gongga-client --lib` (6 passed),
`ship.sh --check` local and remote (both 0), `client_state_selftest.sh -v`
(PASS 4s), `tui_live.py` window-state in default pipe mode (0).
Machine-readable record: `.dev/verify/<sha>-multi-client.json`.

## Still owed

A-4 (`Entry` fold state, `Ledger.pages`), deleting the
`GONGGA_TUI_IN_PROCESS` fallback and the TUI's `Frontend` / `WindowFacts` /
`WindowView` impls, and the fake-provider problem that keeps model-message
tui_live scenarios from being usable as gates.
