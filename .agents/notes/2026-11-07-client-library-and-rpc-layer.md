# The client is a library, rpc is a layer

2026-11-07. The user rewrote `SPEC.md`'s `## Crate Structure` twice. Final shape,
with brackets marking crates / executables:

```text
[protocol]  library  -- cmd schema on the wire, semantic commands
[server]    binary   -- defines and publishes the cmd schema
rpc         layer    -- sending a server command checked under the schema
  [cli]     python
[client]    library  -- receives deltas, validates at reconnect, updates its own
                        state, produces the lines a front-end renders
  [tui]     binary
[bundle]    binary
[...]       more crates may be added
```

`rpc` is the only top-level entry without brackets: a responsibility, not a crate.
That answered both questions left open by the WebUI plan: `client` is a library
crate, and `[tui]` is one binary under it, so `[webui]` is the second binary, not
a second client.

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
`State::load` is still the only reader/writer of `tui.toml`, a one-shot
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
