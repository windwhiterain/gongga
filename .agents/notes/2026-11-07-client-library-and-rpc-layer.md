# Client architecture: library, rpc layer, and daemon

2026-11-07. The user rewrote `SPEC.md`'s `## Crate Structure` three times. The
third changed `[client]` from `libary` to **binary**, and the user then picked
shape 2: the client is a separate process, and front-ends in any language reach
it over a serialized rendering protocol. Current shape, with brackets marking
crates / executables:

```text
[protocol]  library  -- cmd schema on the wire, semantic commands
[server]    binary   -- defines and publishes the cmd schema
rpc         layer    -- sending a server command checked under the schema
  [cli]     python
[client]    binary   -- receives deltas, validates at reconnect, keeps its own
                        state, produces the lines a front-end renders
  [tui]     binary
[bundle]    binary
[...]       more crates may be added
```

`rpc` is the only top-level entry without brackets: a responsibility, not a
crate. The first two revisions named `[client]` a library crate; that reading is
kept below as history. Under the third, the client is a daemon and `[tui]` is one
front-end beside a future `[webui]`, both speaking the wire instead of linking
the library. `crates/client` still ships as a library today -- the binary target
is owed. See `## Shape 2 verdict` at the end.

## Verdicts

- The `Client` state machine and reconnect validation belong to `[client]`, not
  to a front-end. A second front-end must not reimplement sequence-hole
  detection or the greeting resync.
- `render` in `[client]`'s description means producing `Sink` events; each
  binary draws them. `crates/client/src/lib.rs:424-526` already publishes that
  vocabulary without a terminal type.
- Landing path chosen by the user: rename `crates/link` -> `crates/client`
  (`gongga-client`), keep `rpc` as a layer inside that library, do not create a
  `crates/rpc`. The rename landed in stage 1 (see below).
- Stage 0 was documentation only and is committed as `aaecc23`. `SPEC.md` is
  the user's file and was not edited.

## Defaults taken (say so if any is wrong)

- The one-shot `gongga-cli` dispatch at `crates/tui/src/main.rs:14-16` moves out
  when `tui` becomes a drawing-only binary; its destination is stage 2.
- Client state (sticky, shown, remembered address) is per front-end instance on
  separate files; a one-shot cli still reads and writes none. This relaxes the
  2026-11-05 verdict "only the TUI may hold client state" while keeping its
  point: an agent's command must not change the user's window.
- `[webui]` defaults to `--client_only`: it neither spawns nor cancels a server
  it did not start. `--parent` ownership stays as it is.

## What stage 0 changed

- `DESIGN.md` §1: a current/target section with both crate tables and the
  mapping; the old `cli` narrative is now headed as history. Two stale claims
  were corrected: the dependency-direction `include_str!` assertions no longer
  exist (only `server/pylib.rs` x3, `server/ship.rs`, `skill/entry.rs` use it),
  and "SPEC says tui: transparent cli wrapper" is now marked as old SPEC wording.
- `docs/notes/README.md`: `### client` describes the actual library; `### cli`
  points at `crates/client/src/lib.rs` + `crates/tui/src/main.rs` instead of the
  removed `crates/cli/src/bin/gongga-cli.rs`; a "Naming debt" section lists what
  stage 1 must clear and which `cli` names must not be blind-replaced.

## Still owed

- `SPEC.md` (the user's file, untouched): `libary` -> `library`; `[...]` carries
  brackets although it is not a crate; the `bundle` line still says
  `server/cli/tui's binary`, and `crates/bundle/build.rs:93` builds only
  `gongga-server` + `gongga-tui`.
- `SPEC.md` `## UI` (98-144) is a terminal screen spec, and `## Cmd`'s six
  client commands (178-187) still assume one window. Decide how a second
  front-end reads them before stage 3.
- `DESIGN.md` §5's "CLI (command layer)" heading still says `CLI`; the
  `.agents/notes/2026-11-05-*` notes keep `link` on purpose, as history.
- Still to do after the rename: uplift the front-end-independent runtime from
  `crates/tui/src/run.rs`, then add `crates/webui` and wire it into `bundle`.

## Verified (stage 0, documentation only)

`git diff --check` 0 · `cargo build` 0 (`dev`, 2m08s; pre-existing warnings:
four crates with one warning each, and three "error deleting lock file ...
os error 5" on `target/debug/incremental` -- Windows file locks, not a build
failure) · `cargo fmt --check` 0 · `comments-check` 0 · `test-layout-check`
8 packages, 0 violations · `scripts/skill_contract.sh` PASS 8 ·
`scripts/ship.sh --check` "in agreement", protocol content 4 / build 4,
ship HEAD 9dfad43598646f87c767fbfe4ecc58925757d32d (local).
No L3 process test was run: no Rust source changed.

## Stage 1: the crate rename

`crates/link` -> `crates/client` (`gongga-link` -> `gongga-client`), the six
`link-*.md` notes -> `client-*.md`, and every `link::` / `crates/link` /
`cli-lib.md` reference repointed. `gongga-cli`, `cli.toml` and the `cli` clap
layer word were left alone: they are names, not the crate. The historical
`.agents/notes/2026-11-05-*` notes keep `link` on purpose.

Verified: `cargo build` 0 · `cargo fmt` then `cargo fmt --check` 0 ·
`comments-check` 0 (one `# Invariant` section grew to 403/400 after the longer
name; shortened in `crates/client/src/lib.rs`) · `test-layout-check` 8 packages,
0 violations · `git diff --check` 0 · `scripts/skill_contract.sh` PASS 8 ·
`scripts/ship.sh --check` in agreement · `gongga-tui --help` 0 ·
`gongga-server --help` 0.

`scripts/client_state_selftest.sh` exit=1: the server could not clone the
`gongga-ship` content repo (GitHub schannel closed abruptly), so it never
listened and the script bailed before its first check. Environmental, not a
rename regression; the script's own trap removed its temporary home. Not re-run.

## Stage 2: window state and client commands move into the client library

`crates/tui/src/state.rs` -> `crates/client/src/window_state.rs` and
`crates/tui/src/command.rs` -> `crates/client/src/window_command.rs`. Both are
now `pub mod` in `crates/client/src/lib.rs`; `crates/tui/src/run.rs` imports
them as `client::window_command as command` / `client::window_state::State`.
Inside the moved files `client::` became `crate::`. No behaviour changed:
`State::load` is still the only reader/writer of `client.toml`, a one-shot
`gongga-cli` still reads and writes none of it, and the six client commands are
still parsed by the window and never reach the command layer. The notes keep
their `tui-*.md` names -- they are about the window's state, not a crate -- but
the README index now points at their new source paths.

Another agent was editing the same worktree at the same time: it added
`crates/tui/src/closing.rs`, `docs/notes/tui-closing.md`, a `windows-sys`
dependency, and closing-related hunks in both `crates/tui/src/lib.rs` and
`crates/tui/src/run.rs`. Staging `run.rs` whole swept those hunks into
`735cc41`, while `lib.rs` deliberately had no `mod closing;` -- an inconsistent
tree. An isolated `git archive` + `cargo check` caught it (E0432 unresolved
`crate::closing`). The follow-up commit rebuilt `run.rs` from `e20b741` with
only the three import changes, leaving the concurrent agent's work in the
working tree untouched. Neither of its new files was ever staged here.

The working tree is still red from that concurrent WIP: `cargo build` 1
(E0433 x2 / E0408 x3, all in `closing.rs`) and `comments-check` 1 (three
preamble sections over 120 chars, all in `closing.rs` or the closing hunk of
`lib.rs`). Neither failure belongs to this split.

Verified on an isolated export of the corrected tree (`git write-tree` ->
`git archive`, its own `CARGO_TARGET_DIR`): `cargo build` 0 (53.56s) ·
`cargo check -p gongga-client -p gongga-tui` 0 · `cargo fmt --check` 0 ·
`comments-check` 0 · `test-layout-check` 8 packages, 0 violations ·
`gongga-tui --help` 0. In the working tree: `scripts/skill_contract.sh` PASS 8 ·
`scripts/ship.sh --check` in agreement · `git diff --check` 0.

Owed: once the concurrent change lands, re-run the gates on the real tree and
run `scripts/client_state_selftest.sh` (blocked on GitHub so far) to prove a
one-shot `gongga-cli` still reads no window state.

## Stage 3 design contract (pending approval)

`DESIGN.md` §1 now carries the front-end-independent runtime design: what moves
out of `crates/tui/src/run.rs` into `crates/client`, two candidate interface
shapes (A: one `Frontend` trait owning the loop; B: only a shared connection
state machine), the invariants a WebUI must keep, and a staged migration. Marked
【待确认】 -- no Rust code was touched in this turn.

Why no code: another agent still owns `crates/tui/src/run.rs` in this shared
worktree (its console-closing work is uncommitted), and staging that file whole
already corrupted one commit this session. C: has 13G free of 476G (98% used),
so a new worktree with its own `CARGO_TARGET_DIR` was not created either. 19
worktrees are registered; this is the one being written to concurrently. No
disk cleanup was run: that needs the user's confirmation.

The shared worktree compiles again: `cargo build` 0 (1m55s) · `cargo fmt --check`
0 · `test-layout-check` 8 packages, 0 violations · `git diff --check` 0 ·
`scripts/skill_contract.sh` PASS 8 · `scripts/ship.sh --check` in agreement.
`comments-check` 1: three preamble sections over 120 chars, all in the
concurrent agent's `closing.rs` (2) and the closing hunk of `lib.rs` (1); none
belong to this split. No L3 process test was run (GitHub still blocked).

## Shape 2 verdict (2026-11-07, third SPEC revision)

The user's third revision changed `[client]` from `libary` to `binary`, then
answered "2" to the shape question: the client is a standalone daemon and a UI
in any language reaches it over a serialized rendering protocol. This supersedes
the in-process `Frontend` trait (shape 1). `DESIGN.md` §1 keeps that design as
history and adds the shape 2 contract; this note's title changed with the verdict.

Landed this turn (stage 3, step 1): `crates/client/src/render_wire.rs` defines
`RenderMessage` (the 12 drawing methods of `Sink`), `CursorMessage` (the 5
journal methods, never sent to a front-end) and `UiIntent` (4 gestures), all
serde internally tagged; `WireSink` implements every `Sink` method and overrides
`tool_call` so a structured `Head` survives instead of being flattened to text;
`Listed`, `Layout`, `QuotaRow` gained serde derives. No connection loop, process
or ownership invariant was touched. `docs/notes/client-render-wire.md` is the new
note; README and DESIGN §1 were synced.

Six Invariant-level decisions are left for the user, listed as 【待确认】 in
`DESIGN.md` §1: who spawns the server and what `--parent` binds to; bundle
routing; WebSocket vs stdio; moving the window journal into the daemon; which
instance owns `client.toml` (the 2026-11-05 verdict); unifying `UiIntent` with
`tui::UiEvent`. None was assumed or changed here.

`SPEC.md` is the user's uncommitted working-tree edit (the third revision) and
was not staged in any commit on this branch.

Verified: `cargo build` 0 (1m45s, with the standing Windows incremental-lock
warnings) · `cargo check -p gongga-client` 0 · `cargo fmt --check` 0 ·
`comments-check` 0 violations · `test-layout-check` 8 packages, 0 violations ·
`git diff --check` 0 · `scripts/skill_contract.sh` PASS 8 ·
`scripts/ship.sh --check` in agreement · `gongga-tui --help` 0. The first
compile failed on serde's internal tag `kind` colliding with a variant field
named `kind`; the tag was renamed to `type`. No L3 process test was run: no
runtime behaviour changed, and the existing L3 script is still blocked on a
GitHub clone. The gates ran on the shared worktree, which also carries another
agent's uncommitted picker changes in `crates/tui/src/lib.rs` and
`docs/notes/tui-cover.md`; neither file was staged or touched by this turn.

## Six verdicts for shape 2 (2026-11-07)

The user answered all six 【待确认】 items in one message:

1. client spawns the server.
2. TUI goes into bundle, other UIs do not; yes to `gongga tui` becoming two steps.
3. Single UI: one client per UI, otherwise several clients.
4. The window journal belongs to the client.
5. Rename the state file to `client.toml`.
6. Agreed to unify `UiIntent` and `UiEvent`.

Consequences in `DESIGN.md` §1: the "one daemon broadcasts to many front-ends"
design is gone. No broadcast Sink, no multi-connection backlog, no replay
snapshot for a late joiner; several UIs mean several client processes, each with
its own `client.toml`, hole detection and transcript refill. Transport is derived
as stdio JSON Lines (one-to-one, no port or loopback auth) and marked 【待确认】
because the user did not name it. Rulings 1 and 2 conflict with `SPEC.md`'s
`tui [--client_only]` and bundle lines; SPEC is the user's file and was not edited.

Landed this turn -- two mechanical changes, no runtime behaviour:

- `client.toml`: 48 replacements across 18 current source/doc files, including
  `scripts/client_state_selftest.sh` (its hard-coded home file and check text,
  `bash -n` 0). Dated historical notes keep `cli.toml`; they are records. The
  only remaining `tui.toml` is README's naming-debt chain.
- `UiIntent` / `UiEvent`: `UiIntent` gained `Eq`, variant docs and an Invariant
  section; `tui::UiEvent` is `Intent(UiIntent)` or `Recomplete`; every
  construction site and `run.rs` match arm was rewritten. `Recomplete` stays in
  the terminal and never crosses the wire. README's naming debt is marked cleared.

Verified: `cargo build` 0 (1m55s, standing Windows incremental-lock warnings) ·
`cargo check` 0 (18.88s) · `cargo fmt --check` 0 · `comments-check` 0 violations
· `test-layout-check` 8 packages, 0 violations · `git diff --check` 0 ·
`scripts/skill_contract.sh` PASS 8 · `gongga-tui --help` 0 ·
`bash -n scripts/client_state_selftest.sh` 0 ·
`scripts/ship.sh --check --local` in agreement.
`scripts/ship.sh --check` exit=1: MISMATCH, remote `origin/main` is
`de61d29a...` while local `main` is `9dfad435...`. Protocol still agrees
(content 4, this build 4); the remote has a newer content commit this checkout
has not fetched. Not caused by this turn -- no `ship/` file was touched, and no
fetch, rebase or publish was run. No L3 process test was run (GitHub clone still
blocks it). An isolated export of the staged tree (`git write-tree` ->
`git archive`, its own `CARGO_TARGET_DIR`) checked out cleanly: `cargo check
--offline -p gongga-client -p gongga-tui` 0 (1m25s) · `cargo fmt --check` 0.
The committed tree therefore stands without the concurrent picker work.

## Process topology clarified (2026-11-07, 20:21)

Two further user rulings plus one clarification:

- The UI spawns the client binary; the client's lifetime is bound to that UI.
  The client is **not** a resident daemon.
- Only UI <-> client is one-to-one, so a subprocess pipe can replace the local
  socket. Explicitly: **client <-> server is not one-to-one and must be remote**.
  A server serves many clients and may live on another machine; that link stays
  SPEC's reliable UDP. A pipe replaces only the local UI link.

`DESIGN.md` §1 now draws the two links separately and drops the earlier
"stdio derived, 【待确认】" note. Four hazards are recorded: stderr must be
drained or logged (`NullSink::note` uses `eprintln!`); a spawned server already
has its stdio redirected to a log file (`client/server.rs:258`); a synchronous
`WireSink` needs a writer task with a bounded queue, not a blocking write in
the main loop; and the client crate has no TTY probes, so crossterm's real
terminal in the UI cannot collide with the child's pipes.

Existing wheels found, not re-invented: `server/src/remote/transport.rs:334`
(three piped streams + `kill_on_drop`) and `:815-850` (line writer,
`lines().next_line()` reader); `tools/bash.rs`, `tools/python.rs`, `script.rs`
share the shape. `client/server.rs:101` already returns early for a
non-loopback address (`starts_here`, `:205`), so "client spawns server" never
applies to a remote shared server. SPEC still says the *window* spawns and
cancels the server; that wording is the user's file and was not edited.

Open 【待确认】: whose pid goes into `--parent` now that the server is a
grandchild -- the UI's (preferred, keeps the lifetime bound to the UI) or the
client's. No Rust code changed in this turn; documentation only.

Verified (documentation only, no Rust): `cargo build` 0 (1m49s, standing
Windows incremental-lock warnings) · `cargo fmt --check` 0 · `comments-check` 0
violations · `test-layout-check` 8 packages, 0 violations · `git diff --check` 0
· `scripts/skill_contract.sh` PASS 8 · `scripts/ship.sh --check --local` in
agreement. `scripts/ship.sh --check` still exit=1 on the same external
`origin/main` divergence reported in the previous turn (`de61d29a...` remote vs
`9dfad435...` local); no ship file was touched and no fetch/rebase was run.
No L3 process test: no runtime behaviour changed.

## --parent confirmed, pipe frames landed (2026-11-07, 20:34)

"confirm" settled the open question: `--parent` carries the **UI pid**, not the
client pid. `DESIGN.md` §1 records it. The client will take the UI pid from argv
and pass it through `spawn()`, so `whose_server`'s "that window" reading still
holds -- the window is now the parent process.

Landed: `encode_frame` / `decode_intent` in `crates/client/src/render_wire.rs`
-- one JSON value per line, the newline outside the value, both returning a
`Result`. `docs/notes/client-render-wire.md` adds "Frames on the pipe", which
explains why the writer is not a `Sink` (a synchronous `&mut` blocking write
would stall the state machine) and that the runtime needs a bounded queue with
`try_send`. No binary target yet: the frame functions are complete on their own,
but a bin that cannot reach a `Runtime` would be an empty shell.

Disk: the clean-disk script could not be run. Bash -> powershell was blocked by
the tool's security policy ("Invoking PowerShell from Bash bypasses PowerShell
security checks"); the PowerShell tool then blocked `Invoke-Expression`
("executes arbitrary code"). Per the standing rule no equivalent retry was
attempted. C: fell from 3.4G to 0 during the turn and recovered to 20G after a
concurrent build finished. That transient exhaustion made the first
`cargo build` exit=101 with `failed to remove file ... 拒绝访问 (os error 5)`
on `libgongga_protocol-*.rlib`; a retry once space returned passed.

Verified: `cargo check -p gongga-client` 0 (6.29s) · `cargo build` 0 (5m51s on
retry; first attempt 101 for the reason above) · `cargo fmt --check` 0 ·
`comments-check` 0 violations · `test-layout-check` 8 packages, 0 violations ·
`git diff --check` 0 · `scripts/skill_contract.sh` PASS 8 ·
`scripts/ship.sh --check --local` in agreement · `scripts/ship.sh --check`
exit=1, the same external `origin/main` divergence as the previous turn.
No L3 process test and no runtime frame round-trip: the two frame functions are
pure and no runtime consumes them yet.

## Stage 4, step 1: connection lifecycle lifted (2026-11-07, 20:59)

The first slice of the 904-line `interactive` moved into
`crates/client/src/runtime.rs`: `RECONNECT_MIN/MAX`, `SPAWN_POLL/ATTEMPTS`,
`starts_a_server`, `whose_server`, `wait_for`, `dial_now`, and the `Server` enum
with `is_ours` / `label`. `crates/tui/src/lib.rs` re-exports `Server`, so
`Screen::server` and `Tui::set_server` are untouched. `run.rs` now calls them as
`runtime::*`; 904 -> 835 lines. Behaviour is unchanged, with one deliberate
interface change: `whose_server` takes `owner_pid` explicitly instead of reading
`std::process::id()`. With `--parent` now naming the UI and the client a child
process, the old comparison would misread a handover as somebody else's server.
`interactive` passes its own pid today (the UI is still this process); argv will
supply it after the split. See `docs/notes/client-runtime.md`.

Verified: `cargo build` 0 (2m38s) · `cargo check -p gongga-tui` 0 (21.37s) ·
`cargo fmt --check` 0 · `comments-check` 0 violations (first run failed: the
two-line `owner_pid` preamble was 133/120 chars, shortened) ·
`test-layout-check` 8 packages, 0 violations · `git diff --check` 0 ·
`scripts/skill_contract.sh` PASS 8 · `scripts/ship.sh --check --local` in
agreement · `scripts/ship.sh --check` exit=1, the same external `origin/main`
divergence. No L3 process test: no runtime behaviour changed.

## Stage 4, step 2: input translation lifted (2026-11-08)

The main checkout had been left on `feature/client-split` since 17:22 on
2026-11-07; the user caught it and it is back on `main` (pointer fast-forwarded
to the same tree, no file rolled back and no commit lost). This step was done in
`.worktrees/client-split` on `feature/client-split` with its own
`CARGO_TARGET_DIR`; the main checkout was not modified.

`crates/client/src/window_input.rs` now owns `classified`, `submission`,
`filled`, `restates`, `follow` and `typed_names`; `crates/tui/src/run.rs` went
835 -> 598 lines. `submission`'s sink became `&mut dyn Sink`; its bad-parse line
goes through `Sink::note`, which is the same `Tui::say` call the old inherent
`TuiSink::say` made. `shell-words` left `crates/tui/Cargo.toml` (the client
library already had it). `sync`, `settle`, `declare`, `answer_*` and the journal
stay behind: they read `ui.stamp_of`, `ui.drawn`, `ui.room_of` and
`ui.take_settled`. See `docs/notes/client-window-input.md`.

Verified in the isolated worktree: `cargo build` 0 (45.94s on retry; first run
101 writing `invoked.timestamp`, the known Windows os error 5) · `cargo check`
0 (11.97s) · `cargo fmt --check` 0 · `comments-check` 0 violations (first run
1: the new module preamble was 121/120, shortened) · `test-layout-check` 8
packages, 0 violations · `git diff --check` 0 · `scripts/skill_contract.sh`
PASS 1 in the worktree, PASS 8 in the main checkout · `gongga-tui --help` 0 ·
`gongga-server --help` 0. `scripts/ship.sh --check --local` 0 (in agreement,
protocol 4/4) and `--check` 1 (`origin/main` vs local divergence, external)
were run read-only in the main checkout because the worktree's `ship/`
submodule could not be initialised: cloning `gongga-ship` failed with schannel
/ empty reply. The same network failure still blocks the L3 process scripts;
no L3 was run, and no runtime behaviour changed. This paragraph was superseded
on 2026-11-08, when the user opened a proxy -- see the next section.

## L3 verified once the proxy was up (2026-11-08)

`http://127.0.0.1:50274` (the tool's built-in proxy) reaches baidu but returns
`CONNECT tunnel failed, response 502` for github.com. The user's proxy on
`127.0.0.1:7890` returns 200 and works for git and for the server's own
content-repo clone. `flm` is at `C:\resource\kualemon-cli\flm.cmd`. With
`http_proxy` / `https_proxy` set to 7890, `ship/` was initialised in the
worktree (checked out `9dfad435...`, remote tip `5a0d615...`) and both
`ship.sh --check --local` and `ship.sh --check` return **0, in agreement**
there (protocol 4/4). The main checkout's older clone still reports the
`origin/main` divergence; no fetch, rebase or publish was run.

`scripts/client_state_selftest.sh -v`: **PASS · 4s · client-state ·
5e337ea**. This is the first real-process check of the rename, `client.toml`
and the six client commands after several turns blocked by the network.

`scripts/tui_live.py --scenario window-state` fails with exit=3: the new
startup cover has no timer (`docs/notes/tui-cover.md`), the harness sends no
key to dismiss it, and the first script line already expects `session(s)`.
Its first frame lands at t=+6380ms, so an immediate `key enter` is lost too.
Driving the shipped scenario through `--script` with `wait 10000`, `key enter`,
`wait 600` prepended returns **exit=0**: every `expect` in `window-state`
(`/set_sticky`, `/set_render`, `/del_sticky`, `sticky to xxx`) passed. That is
the real-window evidence that lifting `submission`, `filled` and `follow` did
not change behaviour. The temporary drive script was removed. Making the cover
dismissal permanent in `tui_live.py` is a test-tool change and needs the
user's permission.

All test processes from this run ended: the only `gongga-server.exe` left are
`C:\Users\17124\.gongga` on port 9700 (pids 24124 / 5172), which are the
user's. Measurements are in `.dev/verify/5e337ea-l3.json`.

## Stage 4, step 3: the journal's data layer moved (2026-11-08)

`crates/tui/src/cache.rs` -> `crates/client/src/window_cache.rs`. `Record`,
`Cache` and `escaped` have no terminal types: only std fs/io, serde,
`crate::LineKind`, `crate::protocol` and `crate::server::alive`. `tui/src/log.rs`
imports `client::window_cache::{Cache, Record}`; `roster.rs` uses
`client::window_cache::escaped`; `escaped` became `pub`. The `Cache` instance is
still created by `log.rs`, so this moves type ownership, not runtime state.
`log.rs` (2475 lines) keeps ratatui `Style` / `Line` / `Span` and `materialize`.

Correction to the previous turn's plan: stage 4 does NOT need a serialized
window-query protocol. Verdict 4 puts the journal in the client, so
`stamp_of`, `drawn`, `room_of` and `floor_of` become local answers once the
window index moves; `CursorMessage` stays client-internal. `DESIGN.md` §1 now
records this instead of the "cross-process query shape" from the last turn.

Verified: `cargo build` 0 (1m27s) · fresh `cargo build -p gongga-tui` 0 with
no warning (the earlier "1 warning" was a stale incremental diagnostic;
`cargo clean -p gongga-tui` freed 664.4 MiB) · `cargo fmt --check` 0 ·
`comments-check` 0 violations · `test-layout-check` 8 packages, 0 violations ·
`git diff --check` 0 · `scripts/client_state_selftest.sh -v` PASS · 4s ·
client-state · ba69520 · `scripts/tui_live.py --script <window-state + cover
dismissal>` 0 · `scripts/skill_contract.sh` PASS 8 · `ship.sh --check --local`
0 and `--check` 0, both "in agreement". Proxy 7890 throughout.

## Stage 4, step 4: the refills lifted (2026-11-08)

`crates/client/src/window_refill.rs` now owns `declare`, `answer_completion`,
`answer_transcripts` and `answer_older`; `run.rs` 598 -> 479 lines. They used to
take `&Tui` / `&mut Tui`; they now take `&dyn WindowFacts` (and `&mut` for
`answer_completion`, the only one that calls `refresh_completion`). `Tui`
implements the trait with five one-line forwarders to methods it already had,
so behaviour is unchanged. This is a local read-only interface, not a
cross-process query protocol: verdict 4 puts the journal in the client, which
will implement `WindowFacts` itself once `log.rs` / `panes.rs` move. `sync`
and `settle` still need `&mut Tui`. See `docs/notes/client-refill.md`.

Verified: `cargo build` 0 (1m07s on retry; first attempt 101 from the known
Windows incremental `os error 5` lock) · `cargo check` 0 (6m12s after
clean-disk removed the cargo registry sources) · `cargo fmt --check` 0 ·
`comments-check` 0 violations (the new module first had an unused
`Client` / `Link` import; removed) · `test-layout-check` 8 packages, 0
violations · `git diff --check` 0 · `scripts/client_state_selftest.sh -v`
PASS · 4s · client-state · 3eff4d2 · `scripts/tui_live.py --script
<window-state + cover dismissal>` 0 · `scripts/skill_contract.sh` PASS 8 ·
`ship.sh --check --local` 0 and `--check` 0, both "in agreement".
Proxy 7890 throughout.

## 2026-11-08: state sync and settled commands

`sync` and `settle` moved from `crates/tui/src/run.rs` into the new
`crates/client/src/window_sync.rs` (run.rs 480 -> 442 lines). Both take
`&mut dyn WindowView` instead of `&mut Tui`; the TUI implements it in `run.rs`
next to `TuiSink`, because `refresh_lines` builds that sink itself. The six
steps of `sync` and the accepted-only rule of `settle` are unchanged.

`WindowView` is deliberately separate from `window_refill.rs`'s read-only
`WindowFacts`, and does not extend it: verdict 4 puts the journal in the client,
which will implement `WindowFacts` locally, while a front-end over the pipe
implements `WindowView` and holds no window facts. Both are local interfaces,
not a cross-process query protocol. See `docs/notes/client-window-sync.md`.

The `log.rs` / `panes.rs` index was not moved this turn: it is a 3000-line split
of journal data from ratatui folding (`Style` / `Line` / `Span`), and `room_of`
depends on frame height. It stays one full step, not a partial move.

Disk was the first action: C: had 1.99 GB free at 100%. `clean-disk.ps1
-NoElevate` reported `freed_bytes` 9,971,073,024 (9.29 GB, 1.99 -> 11.28 GB)
in 361.2s; `recycle_left` 15.90 GB / 365517 files belongs to another account
and needs elevation. It also removed 307 MB of unpacked cargo registry sources,
so `cargo check` rebuilt them (4m13s). No project target outside this worktree
was touched.

Verified: `cargo build` 0 on retry (52.17s; first attempt 101 from the known
Windows `invoked.timestamp` os error 5 lock) · `cargo check` 0 ·
`cargo fmt --check` 0 · `comments-check` 0 violations · `test-layout-check` 8
packages, 0 violations · `git diff --check` 0 · `gongga-tui --help` 0 ·
`scripts/client_state_selftest.sh -v` PASS · 4s · client-state · f138366 ·
`scripts/tui_live.py --script <window-state + cover dismissal>` 0 ·
`scripts/skill_contract.sh` PASS 8 · `ship.sh --check --local` 0 and `--check`
0, both "in agreement".

## 2026-11-08: the first index piece, and a smaller build

`RowIndex` moved from `crates/tui/src/log.rs` to the new
`crates/client/src/window_index.rs` (log.rs 2475 -> 2405 lines). It is a
1-based Fenwick tree with no ratatui, no unicode width and no frame state; the
five methods became `pub` and the `tree` field stayed private. All 14 call
sites are `Ledger.index` inside `log.rs`, so this is type ownership only. It
is the first cut of the index split: `Ledger` / `Log` / `Panes` / `Hit` still
mix journal data with `Style` / `Line` / `Span` folding, and `room_of` depends
on frame height, so they stay one larger step. See
`docs/notes/client-window-index.md`.

The disk loop is broken structurally. Instead of rebuilding a 4.4 GB target
and cleaning it every turn, this worktree now builds with
`CARGO_INCREMENTAL=0 CARGO_PROFILE_DEV_DEBUG=0` and its own
`CARGO_TARGET_DIR`: `cargo check` left 428 MB, a full `cargo build` 1.3 GB.
Debug info and incremental caches are not needed for the L3 scripts, and the
target is kept between turns so the next build is incremental. No clean-disk
run was needed this turn; no project target outside this worktree was touched.

Verified: `cargo check` 0 (2m09s) · `cargo build` 0 on retry (32.74s; first
attempt 101 from the known Windows `invoked.timestamp` os error 5 lock) ·
`cargo fmt --check` 0 · `comments-check` 0 violations · `test-layout-check` 8
packages, 0 violations · `git diff --check` 0 · `gongga-tui --help` 0 ·
`gongga-server --help` 0 · `scripts/client_state_selftest.sh -v` PASS · 4s ·
client-state · b231bbd · `scripts/tui_live.py --script <window-state + cover
dismissal>` 1 on the first run (the server never logged "listening"; proxy
200, ship submodule intact), 0 on the unchanged retry -- transient ·
`scripts/skill_contract.sh` PASS 8 · `ship.sh --check --local` 0 and `--check`
0, both "in agreement". Proxy 7890 throughout.

## 2026-11-08: Ledger, and the loop-lifting contract

`Ledger` moved from `crates/tui/src/log.rs` into the existing
`crates/client/src/window_index.rs`, next to `RowIndex` (log.rs 2405 -> 2377).
All six fields are pure data and all 50 uses are `Log` methods in the same
file, so this is type ownership only. The fields are `pub` as a transitional
state: while `Log` is still in the terminal crate it reaches them directly;
when `Log` moves, they become private again and the operations become
`Ledger` methods. That is written into `client-window-index.md` so the `pub`
is not mistaken for a design.

Scouting `interactive` showed that lifting it is the real gate for the client
binary, but not a one-turn step: it needs a `Frontend` trait (generic, not
`dyn`, because `impl Future` / `impl FnOnce` are not object-safe),
`FrontendEvent` in place of `UiEvent`, a no-self `closing()` to avoid two
borrows of the view inside `tokio::select!`, and `with_sink` to build the
temporary `TuiSink` inside the front-end. The full shape and a two-step
migration are in `docs/notes/client-frontend.md`; step 1 is interface-only,
step 2 is the mechanical move of the loop body. No code from that contract was
written this turn.

Verified: `cargo check` 0 (9.13s incremental, with the kept 1.3 GB target) ·
`cargo build` 0 (24.24s) · `cargo fmt --check` 0 · `comments-check` 0
violations · `test-layout-check` 8 packages, 0 violations · `git diff --check`
0 · `scripts/client_state_selftest.sh -v` PASS · 4s · client-state · 23539d1 ·
`scripts/tui_live.py --script <window-state + cover dismissal>` 0 ·
`scripts/skill_contract.sh` PASS 8 · `ship.sh --check --local` 0 and `--check`
0, both "in agreement". Proxy 7890 throughout.
