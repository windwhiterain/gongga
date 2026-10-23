# Make the test suite finish when it finishes

Status: done, verified on Windows (Git Bash), 2026-10-23.

## What was slow

`cargo test` was not slow — the harness *waiting for it* was.

| run | before | after |
| --- | --- | --- |
| `cargo test --workspace` to a file | 8.7s | 6.2s |
| same, output piped (`\| tail`) | 7.8s … 300s (flaky) | 6.0–6.3s, 6/6 runs |
| `gongga-server` unit tests | 3.86s | 1.8–2.2s |
| `tools::detached` piped | 4s … 60s+ (1 in 8) | 2.2s, 6/6 runs |

The flaky 60s+ was the real cost: every piped run risked blocking until some
child of the test binary exited, because a child that inherits the harness's
stdout pipe keeps the pipe open after the test binary is gone. `timeout 60`
proved it — tests reported `finished in 3.83s` and the pipeline still hung.

## Root cause

`detect_git_bash` preferred `Git\bin\bash.exe`, which is a **launcher**: it
starts `Git\usr\bin\bash.exe` and waits for it. So the pid recorded for a
detached job named the launcher, not the shell doing the work. `stop()` runs
`taskkill /PID <pid> /T /F`; when that raced the launch, the launcher died
before its child existed, the `taskkill /T` found no child to take, and the real
shell carried on — a job that outlived its own `kill_bash` and, with
`stdout` inherited from the test binary, held the harness pipe open. That is
also a production defect: SPEC says `kill_bash` "is the only thing that ends it".

## Changes

- `config::shell_program` resolves a launcher to the shell it would start, and
  both the detected and the configured program go through it, so the recorded
  pid is the process that does the work.
- `tools/testbed::wait_until` polls a condition every 10ms instead of sleeping a
  fixed time: `sleep 1`/`sleep 3`/`sleep 20` in the detached and bash tests are
  gone, and the background-output test no longer polls at 100ms.
- `stopping_a_job_takes_the_tree_with_it` now asserts the recorded process is
  gone, and reads the "work did not finish" flag after the moment it would have
  appeared — the old version passed while a 20s orphan was still running.
- Every test that spawns a shell kills it before returning.

## Follow-ups, not done here

- `AGENTS.md` says `test-layout-check --config test-layout.toml`, but that file
  has never been committed, so the documented command exits 2. The built-in
  policy is clean: `test-layout-check .` → 0 violations. Either drop the flag or
  commit the config.
- `comments-check.toml` was copied from another project: 773 violations across
  44 files at HEAD. This change adds none (per-file counts unchanged).

---

# Comment policy, brought into line (same day)

`comments-check` had 763 violations at HEAD, all `section-too-long` on the
preamble (median 202 chars, max 1710, limit 120). Done in this turn:

- `comments-check.toml`: `.dev` (gitignored scratch and the backup copy of the
  old CLI/TUI) added to `exclude` — it is not this project's source — and the
  `exempt` entry naming `lichen-language-zed/grammars/lichen` dropped, which is
  another repository's path. 763 → 644 violations.
- All 644 rewritten across 36 files: each comment keeps a one-line claim
  (≤110 chars, margin for the counter), prose that carried the argument moved to
  `docs/notes/<crate>-<stem>.md` in code order. `# Invariant` is used only where
  the text really was a precondition or a guarantee; 54 comments ended up with
  one.
- `test-layout.toml` written (the file `AGENTS.md` names, never committed), so
  `test-layout-check --config test-layout.toml` runs: 5 packages, 0 violations.
- `docs/notes/README.md` indexes the 35 notes and lists the old claims that no
  longer match the code, so the next reader finds them instead of trusting them.

Verification: `comments-check` → 0, `test-layout-check` → 0, `cargo test
--workspace` green (215 tests), `cargo fmt --check` clean, `cargo doc` unchanged
(one pre-existing private-intra-doc warning in `tui`). A line-by-line diff over
non-comment lines confirms no code moved; the only non-comment changes in the
tree are the `shell_program` fix and its test imports from the section above.
