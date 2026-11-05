# The window does not link the command line's program

2026-11-05. The user's verdict, twice in the same direction: "since the TUI no longer depends
on the CLI, hand it straight to the CLI" and then "the TUI sends its commands through the
protocol; it does not need `cli` at all".

## Why the sentence was not the whole of the work

`crates/cli` was two things in one library. One was the *program*: `main`, `Args`, the
subcommands, the one-shot path that prints a command's events. The other was the *link*: a
UDP client, the catalog the server publishes, the argument surface, completion, and the
vocabulary a front-end draws (`LineKind`, `Sink`, `Listed`, `Layout`, `QuotaRow`).

`crates/tui` used the second and had to link the first to get it: 90 references, and a binary
carrying `main`, clap and argv for a window that never runs a command line. So "the TUI sends
through the protocol" is not a wire change -- `protocol` already owns the transport
(`connect`, `Channel`, `encode_client`/`decode_server`). What was in the wrong crate was the
*client on top of it*, and the ~700 lines of vocabulary the window renders.

Doing it the other way -- the window speaking raw `protocol` with its own decode loop -- would
have duplicated that vocabulary, which this repository's own rules forbid ("never duplicate
code"). One client, two front-ends.

## What the crate graph is now

```
protocol ──▶ link ──▶ cli   (the program: one command line, its events out)
                 └──▶ tui   (the window: draws what the same link decodes)
```

- `crates/link` (package `gongga-link`, alias `link`): `lib.rs`, `catalog.rs`, `commands.rs`,
  `complete.rs`, `server.rs`, `skill_command.rs` -- everything the old `crates/cli/src/lib.rs`
  was, and its tests.
- `crates/cli`: `src/bin/gongga-cli.rs` and nothing else. No library, so no lib test target.
- `crates/tui`: `cli::` became `link::` throughout; the invariant on `lib.rs` now says it never
  depends on `cli`.

Notes moved with the code: `docs/notes/cli-{lib,catalog,commands,complete,server,skill-command}.md`
are `link-*.md`, every reference repointed, and the index split into `### link` and `### cli`.

## The merge, and the work it did not eat

`git merge feature/tui-link` was refused twice while another session's uncommitted
`declare_nothing → declare_empty` change sat in `crates/cli/src/lib.rs` and `crates/tui/src/run.rs`
-- the two files the branch rewrites. That change landed as `fd4c2d5` first, and then git's
rename detection carried it into the new home: `declare_empty` is in `crates/link/src/lib.rs:1636`
and its caller at `crates/tui/src/run.rs:164`. Nothing of it was lost, and nothing of it was
committed by me on someone else's behalf.

## Verified at `11bc6ca`

`cargo build --workspace --exclude gongga-bundle` 0 warnings · `cargo fmt --check` 0 ·
`comments-check` 0 · `test-layout-check` 9 packages, 0 violations · `cargo test --workspace`
14 suites ok · `scripts/skill_contract.sh` PASS 8 · `scripts/ship.sh --check` in agreement ·
`scripts/cmd_selftest.sh` PASS · `scripts/client_state_selftest.sh` PASS. The branch's own run
before merging was the same set plus `gongga-cli --help`, `gongga-cli skill ls` and
`gongga-tui --help` smoke-tested, and `link`'s 15 unit tests.

## Still owed

`SPEC.md`'s `## Crate Structure` has no `link` line yet: *the client side `cli` and `tui`
share -- the wire client, the command surface, and the lines a front-end draws*. SPEC is the
user's to change, so it is not in this commit.

A window given a command line still runs it in-process (`link::main`) rather than handing the
process to `gongga-cli`. That is now the same library, not a second program; forwarding it to
the program would be a separate, smaller change.
