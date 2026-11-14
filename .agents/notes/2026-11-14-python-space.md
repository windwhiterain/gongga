# Python path space: write/edit/read reach the session's remote

2026-11-14, session `remote_file`. Worktree `.worktrees/python-space`, branch
`feature/python-space`. WIP.

## What the user decided

- The Rust layer already has one path space (`Space`, `crates/server/src/space.rs`):
  one value decides the machine, `run` is the primitive. The live `read/write/edit`
  are the Python `file` skill (`ship/skills/file/__init__.py`), local-only. Build the
  same seam in Python so the three reach a remote with one switch.
- Fork resolved: **B** -- byte primitives in the Space (`read_bytes`/`write_bytes`),
  the matching/BOM/all-or-nothing logic stays local. Not A (ship the skill source to
  the peer's Python).
- Default follows the session's remote (like `exec`), not explicit-only.

## Shape (content-only, no protocol bump)

- `ship/python/gongga_space.py`: the Python `Space`. `local()`/`named(name)`/
  `of_env()`; `run`, `read_bytes`, `write_bytes`, `stat`, `resolve`. Transport wheels
  (`Peer`, `run`, `hello`, `runner_command`, ...) move here from `remote/_remote.py`
  so there is one copy; `_remote.py` imports them.
- `file` skill: `read/write/edit` gain `remote=None, cwd=None`; I/O goes through the
  Space; `_atomic_write` moves into `Space.write_bytes` (local branch).
- Remote write = `Append` to a temp beside the target + one `Open`'d `mv`. Why not
  `cat`: the runner's `Write` never closes stdin (`runner.rs::write`), so a program
  that reads to EOF hangs; `Space::run` only works because its driver reads one line
  and exits. `Append`/`Read` are the runner's own base64 file I/O (binary-safe); the
  shell only renames. `copy_command` warns `cat` through ssh lost bytes -- avoided.
- cwd: the skill gets `GONGGA_REMOTE` but not the session cwd. Read it read-only from
  `<home>/gongga.db` `sessions` row (the `subagent` pattern), content-only. No Rust
  change.
- Second-peer rule mirrors the retired Rust `file_remote`: a stated remote must be
  the session's own (or local/none); else refuse, never a silent local fallback.

## Wire shapes (runner protocol 5)

- `Read{path,offset}` -> `{data:b64, offset:next, eof}`; loop to eof.
- `Stat{path}` -> `{path,exists,is_file,is_dir,size}`; NotFound is `exists:false`.
- `Append{path,data:b64}` -> `{bytes}`; creates parents, raw bytes.
- `Open{shell,cwd,program,args}` -> `{pid,cwd}`; `Write{shell,data:b64}`; events
  `Out{shell,stream,seq,data:b64}` / `End{shell,code}`; `Close{shell}`.
- SSH argv mirrors `endpoint.rs::peer_command`: BatchMode + ServerAliveInterval=15 +
  ServerAliveCountMax=3 + TCPKeepAlive=yes (+ConnectTimeout), port/identity, host --
  peer_quote(runner words). Default runner `gongga runner`.

## Verify

- L3 against the real peer `lichen-vm` (Windows, Admin@10.144.144.2): read/write/edit
  round trip, byte-exact (CRLF/high bytes, sha), BOM, batch all-or-nothing, refusals.
- Local regression: the existing 6-case file selftest.
- `remote` skill still imports + offline functions after the wheel move.
- Publish: `scripts/ship.sh -m`, then `--check`.

## Open

- The live home needs the new content installed (restart/evolve) to use it; this
  session verifies in a temp home / against the peer directly.
