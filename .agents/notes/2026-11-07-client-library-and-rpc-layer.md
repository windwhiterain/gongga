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
