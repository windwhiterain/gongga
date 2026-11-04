# The client-only window keeps the server it was told to use

Date: 2026-11-04. Session `tui_fix`.

The report was `gongga tui --client_only` hangs. Three separate things stood
behind it, and only the third is the one the report names.

- The flag the spec spells `--client_only` was only `--client-only`, so the
  published command was refused before the window opened (`unexpected argument
  '--client_only' found`).
- A window with no server said why once, on the first attempt, and the refresh
  that follows wiped it: the status line carried `offline -- reconnecting
  (attempt N)` and nothing else. A window that waits looks exactly like a
  window that is hung.
- The bundle retires the build before it takes over with the program that
  answers, and the retire asked the old build's server to `cancel`. That is
  the server a client-only window was told to use, stopped on the way in, on
  every upgrade -- which is why the window waited for a server the bundle had
  just killed.

Verdict from the user, after the first round of experiments: *你在反复杀
服务器*. Two runs of the bundle had used the operator's own `GONGGA_HOME`, so
the retire asked the live server to cancel; `~/.gongga/server.log` has it at
03:00:18 and 03:04:38. Every run since names a home under `.dev/`, and the
cleanup kills by pid, matched on the home in the command line.

Fixed in `b7715c2`:

- `cli::Args::client_only` is `--client_only`, with `--client-only` as an
  alias, so the name the spec publishes is the name the binary takes.
- The window says why it has no server on *every* attempt, not on the first:
  `nothing is listening at 127.0.0.1:9700 (attempt 3) -- still trying; Alt+Q
  leaves`. The status line holds the newest thing the window knows, so a note
  said once is a note nobody sees.
- The bundle's retire leaves a server alone when the program about to run
  starts none (`keeps_servers`), so a client-only window keeps the server it
  was told to use.

`scripts/bundle_selftest.sh` covers the third one, and `scripts/tui_live.py`
gained `--client-arg` so the second one is drivable with the harness the
repository already has.

Not done (needs the user's call):

- `cli::spawn`'s error is dropped by `.ok().flatten()`, so a window whose
  server cannot be started says `nothing is listening` instead of saying why
  the server did not come up.
- A client-only window still claims an unowned server as its own, and the
  server follows the window: closing such a window stops the server it was
  told to use. That is `server-signal.md`'s rule ("a window that connects to
  an unowned server takes it"), and it is the opposite of what a guest wants.

Verified: `cargo build --workspace`, `cargo fmt --check`,
`cargo test --workspace` (8 passed), `comments-check`, `test-layout-check`,
`scripts/skill_contract.sh`, `scripts/ship.sh --check` (in agreement),
`scripts/bundle_selftest.sh` (PASS), and
`scripts/tui_live.py --no-server --client-arg=--client_only`, whose frames
carry the reason. The selftest was mutated once -- the carve-out reverted --
and the scenario it guards went red.
