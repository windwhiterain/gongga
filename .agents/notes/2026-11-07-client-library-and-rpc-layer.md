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
  binary draws them. `crates/link/src/lib.rs:424-526` already publishes that
  vocabulary without a terminal type.
- Landing path chosen by the user: rename `crates/link` -> `crates/client`
  (`gongga-client`), keep `rpc` as a layer inside that library, do not create a
  `crates/rpc`.
- This turn's scope: stage 0, documentation only. No Rust source and no
  `SPEC.md` were edited.

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
- `docs/notes/README.md`: `### link` describes the actual library; `### cli`
  points at `crates/link/src/lib.rs` + `crates/tui/src/main.rs` instead of the
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
- `DESIGN.md` §5's "CLI (command layer)" heading, and `cli` -> `link`
  references inside individual `docs/notes/*.md`, are cleared with the rename.
- `crates/link` -> `crates/client`, the runtime uplift from
  `crates/tui/src/run.rs`, and the new `crates/webui` are stages 1-3.

## Verified (stage 0, documentation only)

`git diff --check` 0 · `cargo build` 0 (`dev`, 2m08s; pre-existing warnings:
four crates with one warning each, and three "error deleting lock file ...
os error 5" on `target/debug/incremental` -- Windows file locks, not a build
failure) · `cargo fmt --check` 0 · `comments-check` 0 · `test-layout-check`
8 packages, 0 violations · `scripts/skill_contract.sh` PASS 8 ·
`scripts/ship.sh --check` "in agreement", protocol content 4 / build 4,
ship HEAD 9dfad43598646f87c767fbfe4ecc58925757d32d (local).
No L3 process test was run: no Rust source changed.
