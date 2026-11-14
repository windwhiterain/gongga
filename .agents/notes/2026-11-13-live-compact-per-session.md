# Live compact per session, and a config that is read as it is now

Branch `feature/live-compact-session`, worktree `.worktrees/live-compact-session`.
The user asked for two things in one line: make live compact a per-session parameter,
and make every config file live tracked (an edit applies without a restart).

## Landed: `2f79d299`

- SPEC: `SessionConfig.live_compact := {enabled, interval_tokens}`, each unset field
  following the home's `[agent]`; the Live Compact section says so.
- `sessions.live_compact` / `live_compact_interval_tokens` are NULL for "follow the
  file", so a session that never asked is indistinguishable from a row written
  before the columns existed.
- `set_session --live_compact_enabled/--live_compact_interval_tokens` (and the nested
  `--config` spelli ng) persists, applies, opens the checkpoint at the transcript's
  end when switched on, and drops a queued request when switched off.
- The tool head is written once per session, so the schema can no longer follow the
  switch: `stamp()` always carries `live_compact`. The alternative -- re-stamping on
  a toggle -- is the 85k-98k-token cold start `docs/notes/server-tools.md` measured.
- `Agent` now holds `Arc<config::Current>` instead of a dozen copied numbers; the
  agent numbers, the `[model]` table, `cache_ttl_ms` and `default_context` are read
  per use, and a cached `LlmClient` is keyed on the config read (`Config::read`) that
  built it.

## Landed after the first two commits: `04729c5`, and the merge `1ca7e82`

- The tool table is `live::Derived<Config, Arc<ToolRegistry>>`, keyed on `Config::read`:
  rebuilt on an edit, not per call. `Agent::tools()` is the one accessor.
- The disk threshold is not kept in the process: `disk::install` places only the home, and
  `note_tool_end` takes the threshold from the `ToolContext` the call was built with.
- Prompt templates needed nothing: `prompts.rs` already re-reads the home's `[prompts]` on a
  stamp change. The shipped half comes from the content checkout, not from a config file.
- `quota_interval` (main's, `[server] quota_interval_secs`) is read at the step end too, so it
  joined the same list in the merge.

The merge with main took six conflicts: `tools/tunnel.rs` and `worker.rs` were deleted on main
(so those edits went with them), the tool table had to build main's new
`ToolRegistry::settings(max_output_chars)`, and the argument list kept main's `config_arg`
rewrite with the two `LiveCompact` arguments appended.

## Still open

1. **Merged into main** as a fast-forward (`f8d31384`), 2026-11-14 12:2x. The main worktree was
   dirty in `SPEC.md` and `crates/server/src/lib.rs`, so that WIP was stashed, main was fast-
   forwarded, and the stash was applied back: `lib.rs` came back whole (their 38 lines), and
   `SPEC.md` had one conflicting hunk -- their `live_compact: bool = false` against this branch's
   `live_compact: LiveCompact` -- resolved to the struct, which carries the interval and is what
   the server implements. Their other hunks (`--check-config`, the `ls_subsscription` typo) are
   intact. The stash is kept as `stash@{0}`.

What is still missing is not a merge: the *running* server is main's older build
   (`target/debug`, `--parent-build 707bb3712076-dirty`), so `/new_session` has no
   `--live_compact_*` to complete until a build of this commit serves the home. That is a restart
   of the home (evolve), which cancels its running turns.
2. `[channel.<name>]` is still read once, when `Channels` is constructed (three processes build
   one, and it holds the channels that are running). Making it live needs `Current` threaded
   into `Channels`; doing it by hand-rolled refresh was judged worse than leaving it named.

## Gates, all run on this branch

`cargo check -p gongga-server` (0), `--tests` (0), `cargo build` (0), `cargo fmt --check` (0),
`cargo test -p gongga-server --lib -- disk:: agent::` (13 passed), `comments-check` (0),
`test-layout-check` (0 violations), `scripts/skill_contract.sh` (PASS), `scripts/ship.sh
--check --local` (0, protocol 11 = 11; a fresh worktree needs `git submodule update --init` and
`git -C ship merge --ff-only origin/main` first, or the hook refuses with a stale content
protocol).

## Machine note worth keeping

Cargo's package cache is per user, not per worktree: one lock at
`~/.cargo/.package-cache` plus one registry. On 2026-11-13, 21:20-22:20, that cache
was missing ~140 crates after a disk cleanup, and every build had to fetch them over
a **direct** link at 62 KB/s while `static.crates.io` through `127.0.0.1:7897` gave
340 KB/s (5.4x, measured with `curl -w`). Seven cargo processes queued on the lock and
five sessions looked frozen; nothing was deadlocked and no code was at fault.
`cargo fetch` takes one second once the cache is whole -- check that before believing
anything else. For builds here, export `HTTPS_PROXY=http://127.0.0.1:7897` and
`NO_PROXY=localhost,127.0.0.1,::1`; the `proxy` skill's own router (7899) is not
running and its rules send everything but `github.com` direct.
