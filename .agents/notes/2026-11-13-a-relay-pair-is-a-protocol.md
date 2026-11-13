# 2026-11-13 — a relay pair is a protocol, not a commit

## What showed up

"relay 又坏了": every `exec` in the live home died. 87 channels created after 22:35 had
`cmd` + `in` + `relay.log` and no `status`, and each `relay.log` was one line:

```
gongga-relay: this is build 596a8c653f0e, and the server that asked is 7837bd0a4105-dirty
```

The server sat 20 s (`START_DEADLINE`) on each start and answered `ok=false` with nothing
else, because the refusal was written to a file the tool never reads.

## Two things were wrong, and the second one was the design

**1. The copy in `bin` stayed old.** `place` writes beside and renames in; a rename over a
file a running relay holds fails on Windows, and `place` answers "a usable old one beats
none" — silently. A relay of build 596a8c had been holding `bin/gongga-relay.exe` since
21:56, so the 22:34 build could not replace it and nobody was told. `relay_for_channel`
then handed that stale path to every new channel.

**2. The guard compared the commit.** SPEC's rule for programs that travel together is
"the latest *compatible protocol*", and `docs/notes/server-channel.md` already said a
layout change "raises `PROTOCOL`". `main.rs` compared `protocol::BUILD` instead, so every
`cargo build` invalidated a pair that was byte-identical where it mattered:

```
git diff 596a8c653f0e..HEAD -- crates/relay     # doc comments only
```

Two relay binaries whose `Words`/`Status` code never changed were refused as a pair.

## What changed

- `crates/relay/src/relay.rs`: `pub const PROTOCOL: u32 = 1;` beside the two structs it
  is about, and `REFUSED = 3` (the code a refusal uses, now shared with the server).
- `crates/relay/src/main.rs`: `--protocol <n>` decides; `--build` is identity for the
  refusal's words only. No flag at all is accepted (an older server asks nothing).
- `crates/server/src/tools/channel.rs`: `start_here` → `launch_relay`, which names the
  protocol; `await_status` takes the relay's `Child` and fails the moment it is gone
  without a `status`, carrying the relay's own last words instead of a deadline.
  A refusal (`2`, `3`, or nothing started) is answered with one `refresh_relay` and one
  retry; when there is nothing to install the start fails at once with those words.
- `crates/server/src/shell_command.rs`: `refresh_relay` — place from beside the server and
  answer `Some` only when `bin` then holds those bytes; never a source for itself (a
  server running from `bin` would "update" a file with itself).
- `crates/server/src/remote/runner.rs`: a peer's relay gets the same two flags.
- `docs/notes/server-channel.md`: "The pair is a protocol, not a build".
- `scripts/channel_pair_selftest.sh`: the four cases below, in one server lifecycle plus
  a home of its own for the third.

## Verified

- `scripts/channel_pair_selftest.sh` — PASS · 26 s: same protocol with another build is a
  pair; no protocol named is accepted; another protocol exits 3 naming both; a bad copy in
  `bin` is replaced and the channel starts; a copy that cannot be replaced fails in ~0.5 s
  (was 20 s) with no channel left behind.
- `scripts/tool_selftest.sh` — PASS · 64 s (a real turn opening and reading a channel).
- `cargo fmt --check`, `comments-check`, `test-layout-check` — 0.
- The live `~/.gongga/bin/gongga-relay.exe` now answers `--protocol`; with 99 it exits 3
  and names both numbers.

## Landed mid-run, not part of this change

The disk was at 66 MB free and a server died with `os error 112`. `clean-disk.ps1
-NoElevate` freed 2.86 GB (`SUMMARY_JSON.freed_bytes`), exit 0, `recycle_left` 0.

## Still open

- **The command layer prints no message for a refused command.** `gongga_cli.py` prints
  `reply.events` and an exit code; `Outcome::err` text is not among them, so a failed
  `new_channel` shows the echo and nothing else. That is why this incident read as
  silence. Fixing it changes every failing command's output — worth its own pass.
- A relay pinned by a running channel still cannot be replaced (Windows). The start now
  says so instead of pretending; a build-named copy (`bin/relay/<BUILD>.exe`) would not
  need replacing at all, if it turns out to matter often.
- `crates/server/src/tools/channel.rs` still leaves the channel directory behind when a
  start is refused (no row, no `status`, just files). Harmless, but it is litter.
