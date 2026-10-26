# Reliability review fixes (WIP)

- User decision: `del_session` deletes all bash for the session, including `bash.detached`; detached remains a bash field. Current SPEC already states this.
- Core approach: make initial turn row + first user message one SQLite transaction; deliver inject rows + transcript appends in one transaction; finish status + owned pending deletion in one transaction.
- Scheduling: use a preclaimed session gate for every spawned turn; one recovery owner per session; ensure an inject-only recovery creates a durable turn marker before delivering inbox.
- Self delete: two-phase delete for harness caller, with final cleanup after the current turn releases its gate. Cleanup includes persistent and detached bash.
- Parallel scoped work: protocol channel, TUI input/restore, LLM retry parsing, bash output/background protocol.
- Verification done (this session): `cargo test --workspace` 381/381, `test-layout-check` 0 violations,
  `comments-check` 0 violations (50 preamble over-lengths shortened; the prose that could not fit was moved
  into `docs/notes/{server-net,server-tools-bash,server-tools-detached,cli-lib,cli-commands,tui-log}.md`).
- The tree did not compile when taken over: `Dispatch` gained `shutting_down` (commands.rs) with no
  constructor updated and no reader. Now set by `cancel` before the notice window, read by `spawn_turn`
  (a turn started in the window leaves its durable row for the next start instead of racing the drain)
  and by `nudge_inboxes`; `ActiveGuard` is taken at the spawn site, not inside the task, because the
  drain reads `active` and a queued-but-unpolled task would let the process exit mid-turn.
- Fixed with the user's ruling: `set_event_split --split <bool>` implemented (cli state, `Sink::split`,
  `Log::set_split` stickiness) and mode `one` deleted from `DETAIL_MODES`/`Detail`; SPEC.md:66 typo
  `set_event_plit` -> `set_event_split`. `del_session` stays fail-closed on an un-stoppable detached
  bash (user's choice, SPEC unchanged).
- Fixed without ruling (code contradicting SPEC/AGENTS): detached job and persistent shell could share a
  name, breaking the `{session, bash_name}` key (SPEC.md:20) -- `DetachedRegistry::holds` plus the shell
  namespace are now both checked at start; a shell handed to the background escaped every teardown
  (AGENTS.md), now retired pids are kept and ended by `close_shells`; the background sentinel was a
  guessable counter, now a uuid like the foreground one; `del_bash`'s tool description said detached
  jobs were untouched while the code stops them.
- Removed dead code the revision left behind: `Store::touch_session` (finish_turn writes `updated_at`).
  Still warned: `SessionManager::materialise` (test-only now) and `PendingTask.payload`/`created_at`
  (read only by tests) -- left alone, deleting them reaches into test fixtures.
- Smoke run against an isolated home/port: turn row + transcript persisted, interrupt dropped the owed
  task, `cancel` reported it preserved, the server drained and released its lock, and a restart resumed
  it (`resuming turns left pending by the last run resumed=1`). `.dev/smoke*` scratch left in place.
- Open, reported not fixed: `a_failed_database_insert_reclaims_the_spawned_process` flakes under machine
  load (passes 4/4 quiet runs); bridge's recovery notices are best-effort `let _ =` (a failed "result
  unknown" ping silently strands the user); `channel.rs` still has `max_retransmits: 12`, which AGENTS.md
  forbids as a limit -- pre-existing, not from this revision.
