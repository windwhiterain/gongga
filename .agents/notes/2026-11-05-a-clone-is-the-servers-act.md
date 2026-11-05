# A clone is the server's act, because only the server can say it happened

Date: 2026-11-05. Session `heir`.

The report: *目前 heir 是手动复制数据库 clone session，导致服务器无法发信息同步
client，SPEC 新增 clone session 命令*. The heir's `transcript` script copies its rows
into a mentor session with plain `insert`s. Nothing announces the result: the window
tree is drawn from the `sessions` event, so the mentor exists in the database and in
no client. The copy was correct and invisible, which is the one failure a script
writing a shared database cannot fix on its own -- only the process that owns the
wire can.

`clone_session <session> <name>` is the server's own act now:

- `Store::clone_session`: one transaction, two statements. `INSERT INTO sessions ...
  SELECT` copies the source's own facts (model, placement, tuning, last measurement);
  `INSERT INTO messages ... SELECT` copies the rows at their own `seq`. No row is read
  into Rust and written back, so a copy taken while a turn appends holds every row or
  none, and the source cannot lose one. `status` is the only column not copied -- it
  says what the session is doing now, and a copy is doing nothing.
- The command publishes `Event::Sessions` and then every copied row as the event it
  streamed as (`row_event`, `REPLAY_TURN`), on the bus rather than in the answer: a
  read is one client's private question, a clone is a session every tree should hold.
  A failed re-send is a warning in the log, never an undo.

Verdict from the user, answering what a clone is: *完全相同*. So the copy keeps the
prompt row (`system`) too -- the clone reads row for row what the source reads, at the
same `seq`, rather than re-rendering its own head and renumbering. `context_tokens` and
the rest of the last measurement travel with it for the same reason: they describe a
transcript the copy now holds. A turn the clone runs later is still its own.

Refusals, both structural: the source must exist, and the target name must be free
(`UNIQUE`, reported as `already exists`) -- a copy that joined an existing transcript
would be two conversations under one window, the reason `set_session --must_create`
exists for the same class of caller.

Verified: `cargo build`, `cargo fmt --check`, `cargo test -p gongga-server` (8 passed),
`comments-check`, `test-layout-check`, `scripts/skill_contract.sh`,
`scripts/ship.sh --check` (in agreement), plus two real-process runs:

- a temporary home and a real `gongga-server`: five rows written into `main` (including
  a `system` row), `clone_session main copy` answered `cloned 5 row(s)`, the two
  sessions compared equal in sqlite on `seq/kind/content/display/call_id/created_at`
  and on the settings columns, `get_messages --session copy` read them back, and the
  three refusals (taken name, unknown source, nothing written by either) held.
- `scripts/tui_live.py` with a real window: one turn, then `/clone_session main copy`
  typed into `cmd_input`; `copy` appeared in the tree and its pane drew the copied
  conversation. This is the sync the report asked for.

Left open, for the user:

- The heir's `transcript.py` (ship content repo) still uses its own inserts. Switching
  it to `clone_session` needs a design for the rest of its transaction -- the marker
  row's place in the order, and the `DELETE FROM messages` that empties the source,
  which SPEC still gives no command. Not part of this delivery.
- `gongga-cli` exits at the `cmd_result`, so the row events a clone publishes can reach
  it after it has already left. The window is a long-lived client and is the one the
  sync is for; nothing here depends on the asking CLI drawing them.
