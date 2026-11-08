# Step ⑤: the client bin speaks the pipe

2026-11-08, in `.worktrees/client-split` on `feature/client-split`. The main
checkout is left alone: it is on `main` at `f3826a5` and has an uncommitted
`SPEC.md` belonging to another agent.

## What landed

- `crates/client/Cargo.toml` declares a `gongga-client` bin beside the library.
- `crates/client/src/main.rs` is the whole process: parse argv, one stdout writer
  task, `BufReader(stdin).lines()`, `PipeFrontend`, `runtime::run`.
- `Args::parent` is a hidden global flag. `runtime::run` now passes `owner_pid`
  to `spawn()` instead of `std::process::id()`, so the server follows the UI.
- No vocabulary change in `render_wire.rs`, no new terminal type in the library.

## Verification done before committing

- `cargo fmt --all`, `cargo build` (separate target dir), `cargo fmt --check`.
- Real pipe runs in a throwaway home: quit, EOF, malformed line, older, and a
  submit whose text contains a newline. All `exit=0`, stderr empty, every stdout
  line valid JSON; the newline stayed inside one frame.
- `comments-check` (0 violations), `test-layout-check`, `git diff --check`,
  `scripts/skill_contract.sh`, `scripts/ship.sh --check --local` (0), the L3 state
  selftest (`PASS · 4s`) and `scripts/tui_live.py` window-state (0). Recorded in
  `.dev/verify/<sha>-pipe.json`.
- `scripts/ship.sh --check` (with the remote) returned **3**: 7890/7897 refused
  the CONNECT tunnel; 1080 answers a direct `git ls-remote` but the script's own
  `ls-remote` stays empty on every attempt. That is "cannot tell", not a protocol
  drift; `--check --local` is green and the pre-commit hook passed.
- The L3 state selftest needed a stale `.dev/selftest/state-home` cleaned first:
  its `server.lock` still named pid 20128 with a watchdog restarting on port
  60004. Killed only that home's watchdog trees by pid (11332/20128 before,
  13940/20864 after the passing run), never by image name; the user's `C:\resource\gongga\target\debug`
  servers were left alone.

## Deliberately not done

Step ⑥ (TUI spawns this binary) and ⑦ (bundle payload white-list) are untouched:
the new bin is not on the operator's path yet. `WindowFacts` is answered from the
frames the process produced, not from a client-owned journal; `room_of` is 0 and
`known_list` is empty. Four control frames are still owed (completion, server
ownership, repoint, draft). See `docs/notes/client-pipe.md`.

The feature branch and `main` still diverge from `911000b`: feature has the
client-split series, main has `f3826a5` (server signal / build naming). No merge
was attempted — that is the user's call. `735cc41` still carries a stray WIP hunk
in the middle of the series; a squash is likewise pending permission.
