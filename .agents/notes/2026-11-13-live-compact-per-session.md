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

## Left

1. The tool registry (`[tool.*]`, `[channel.*]`, `[bash]`, `[python]`, `[skills]`,
   `max_wait`) is still built once at startup. Rebuild it when the config read
   changes (`Arc::ptr_eq` on `Current::now()`), not per call.
2. `disk::install(cfg.disk_low, ...)` holds a process-wide threshold; read it at use.
3. `prompts::shared()` returns `&'static Prompts` from a `OnceLock`. Prompt rows are
   written once per session, so templates only have to be current when a row is
   written: install them from `Current::now()` on the read that changed.
4. Gates not yet run on this branch: `cargo fmt --check`, `comments-check`,
   `test-layout-check`, `scripts/skill_contract.sh`, `scripts/ship.sh --check`.

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
