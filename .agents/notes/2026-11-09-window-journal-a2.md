# A-2: the disk journal is the client's (2026-11-09)

Step A-2 of `DESIGN.md` §1 "裁定 A". All work in `.worktrees/client-split`; the
main checkout was never touched.

## What changed

- `client/src/window_journal.rs` now owns a `Cache`. `new()` uses `Cache::none()`,
  `with_cache(root, endpoint)` and `repoint(root, endpoint)` build/switch it.
  `cache()` gives a view read-back path; `stamp`, `rewind`, `stamp_offset`,
  `stamp_of` and `floor_of` are the authoritative methods. `consume` writes the
  cursor half down: `Transcript` -> `Stamp`, `Landed` -> `Floor` + `Stamp`,
  `Rewind` -> `Rewind`. `stamp_of` reads the disk first, so a fresh process
  answers with the same number.
- `client/src/window_roster.rs` is `tui/src/roster.rs` moved into the library
  (pure file IO, no terminal type). `tui/src/roster.rs` is now a one-line
  re-export, so TUI call sites are unchanged.
- `tui/src/log.rs`: the `cache: Cache` field is gone. `Log` holds one
  `WindowJournal`; every `self.cache.*` now goes through `journal.cache()` or a
  client-side authoritative method. `Log::stamp` and `Log::rewind` delegate;
  `Log::stamp_offset` was deleted (`rewind` returns the cut). One destructure in
  `Log::window` was updated to bind `journal` instead of `cache`.
- `tui/src/panes.rs`: its own `WindowJournal` field is gone; all nine uses
  forward to `self.log.journal()` / `journal_mut()`. `end_frame` snapshots
  `drawn` first because it also calls `log.evict` mutably.
- `client/src/main.rs`: the UI factory now uses the `client_home` and `endpoint`
  arguments it was already given, to build `WindowJournal::with_cache` and
  `Roster::new`. `known_list()` reads the roster, `repoint()` re-points both,
  `Layout` frames write the roster, `Forget` frames clear the journal and its
  files. `--parent` and every other pipe decision are unchanged.

## What was deliberately not done

`Entry` was not split. `Log::landed` needs `Entry::seal_record` and `materialize`
(which reads cold bodies back through ratatui folding state), so a whole-file
move would touch ~2377 lines in one step with no small verification point.
`Ledger`'s type is in the client already; its `pages` / `page_first` instances
still live in TUI's `views`. Both are A-4, and neither blocks A-3: the pipe
front-end has no blocks to rewrite and answers a refill from appended records.
The four control frames (completion vocabulary, server ownership, repoint
cleanup, draft restore) are still owed before A-3.

## Verification

`cargo fmt --check`, `cargo check`, `cargo build`, `comments-check` (one 128/120
preamble in `main.rs` shortened), `test-layout-check`, `git diff --check`,
`skill_contract.sh`, `ship.sh --check --local` and `--check`,
`cargo test -p gongga-client --lib` (3 tests: cross-restart stamp/floor, repoint
forgetting, roster surviving a process), `client_state_selftest.sh -v`, and
`tui_live.py` window-state + a no-LLM wheel script.

Real pipe process under `.dev/pipe-a2`: quit and a four-intent run both exited
0 with empty stderr and every stdout line valid JSON. Empty `Layout` writes no
roster file, which is `Roster::know`'s intended no-op; the disk persistence path
is covered by the unit tests instead.

Known unrelated issue (unchanged since the main merge): `tui_live.py` scenarios
that send a model message cannot pass, because the server reads only the
community `providers.toml` and falls back to a real provider instead of the fake
roster. Do not use those scenarios as a gate until provider discovery is fixed.

Machine-readable record: `.dev/verify/<sha>-journal2.json`.
