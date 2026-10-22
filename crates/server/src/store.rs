//! Everything durable lives in SQLite.
//!
//! The SPEC asks for data that is "structured for agentic retrieval/mutation",
//! so the transcript is not an opaque JSON blob: each session, message, tool
//! call and event is an addressable row with typed columns. A future tool can
//! therefore `SELECT` a session's tool failures or rewrite one message without
//! parsing and re-serialising a whole document.
//!
//! The connection is behind a mutex and used synchronously. Every statement here
//! is a sub-millisecond local write, so the simplification costs nothing in
//! practice; if that stops being true the fix is `spawn_blocking`, not a redesign.

use std::path::Path;
use std::sync::{Arc, Mutex, MutexGuard};

use protocol::SessionStatus;
use rusqlite::{Connection, OptionalExtension, params};

const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS sessions (
    id         INTEGER PRIMARY KEY,
    name       TEXT NOT NULL UNIQUE,
    status     TEXT NOT NULL DEFAULT 'idle',
    model      TEXT NOT NULL,
    cwd        TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL,
    -- Set once context compression has folded the older part of the transcript
    -- into a summary. The transcript itself is never rewritten: `summarised_upto`
    -- only says where the summary takes over when building a request.
    summary    TEXT,
    summarised_upto INTEGER NOT NULL DEFAULT 0
);

CREATE TABLE IF NOT EXISTS messages (
    id         INTEGER PRIMARY KEY,
    session_id INTEGER NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
    seq        INTEGER NOT NULL,
    kind       TEXT NOT NULL,
    content    TEXT NOT NULL,
    created_at INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS messages_by_session ON messages(session_id, seq);

CREATE TABLE IF NOT EXISTS tool_calls (
    id          INTEGER PRIMARY KEY,
    session_id  INTEGER NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
    call_id     TEXT NOT NULL,
    name        TEXT NOT NULL,
    arguments   TEXT NOT NULL,
    output      TEXT,
    status      TEXT NOT NULL,
    started_at  INTEGER NOT NULL,
    finished_at INTEGER
);
CREATE INDEX IF NOT EXISTS tool_calls_by_call ON tool_calls(session_id, call_id);

CREATE TABLE IF NOT EXISTS events (
    id         INTEGER PRIMARY KEY,
    session_id INTEGER,
    seq        INTEGER NOT NULL,
    kind       TEXT NOT NULL,
    payload    TEXT NOT NULL,
    created_at INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS events_by_session ON events(session_id, seq);

-- Work that was in flight when the process stopped. `cancel` leaves these rows
-- in place on purpose, and the next start replays them.
CREATE TABLE IF NOT EXISTS pending_tasks (
    id         INTEGER PRIMARY KEY,
    session    TEXT NOT NULL,
    kind       TEXT NOT NULL,
    payload    TEXT NOT NULL,
    created_at INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS pending_by_session ON pending_tasks(session);

-- Processes started with `bash --detached`. They outlive the shell, the
-- session and the server itself, so this table is the only thing that lets the
-- next run find -- and stop -- what the last one launched.
CREATE TABLE IF NOT EXISTS detached_jobs (
    id         INTEGER PRIMARY KEY,
    session    TEXT NOT NULL,
    shell      TEXT NOT NULL,
    job        INTEGER NOT NULL,
    pid        INTEGER NOT NULL,
    cmd        TEXT NOT NULL,
    log        TEXT NOT NULL,
    started_at INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS detached_by_session ON detached_jobs(session);
"#;

#[derive(Debug, Clone)]
pub struct Store(Arc<Mutex<Connection>>);

/// Adds columns to a database created before they existed. `CREATE TABLE IF NOT
/// EXISTS` cannot do this, and a running harness must not lose its transcript
/// just because the schema grew.
fn migrate(conn: &Connection) -> rusqlite::Result<()> {
    for (column, ddl) in [
        ("summary", "ALTER TABLE sessions ADD COLUMN summary TEXT"),
        (
            "summarised_upto",
            "ALTER TABLE sessions ADD COLUMN summarised_upto INTEGER NOT NULL DEFAULT 0",
        ),
    ] {
        let exists = conn
            .prepare(&format!("SELECT {column} FROM sessions LIMIT 1"))
            .is_ok();
        if !exists {
            conn.execute(ddl, [])?;
        }
    }
    Ok(())
}

impl Store {
    pub fn open(path: &Path) -> anyhow::Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let conn = Connection::open(path)?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        conn.execute_batch(SCHEMA)?;
        migrate(&conn)?;
        Ok(Self(Arc::new(Mutex::new(conn))))
    }

    fn lock(&self) -> MutexGuard<'_, Connection> {
        // A poisoned mutex means a prior write panicked mid-transaction; the WAL
        // keeps the database itself consistent, so recovering the guard is safe.
        self.0.lock().unwrap_or_else(|e| e.into_inner())
    }

    // -- sessions ----------------------------------------------------------

    pub fn create_session(
        &self,
        name: &str,
        model: &str,
        cwd: &str,
    ) -> anyhow::Result<i64> {
        if name.trim().is_empty() {
            anyhow::bail!("session name must not be empty");
        }
        let now = now_millis();
        let mut conn = self.lock();
        let tx = conn.transaction()?;
        tx.execute(
            "INSERT INTO sessions (name, status, model, cwd, created_at, updated_at)
             VALUES (?1, 'idle', ?2, ?3, ?4, ?4)",
            params![name, model, cwd, now],
        )
        .map_err(|e| duplicate_session(name, e))?;
        let id = tx.last_insert_rowid();
        tx.commit()?;
        Ok(id)
    }

    pub fn delete_session(&self, name: &str) -> anyhow::Result<bool> {
        let mut conn = self.lock();
        let tx = conn.transaction()?;
        let Some(id) = tx
            .query_row(
                "SELECT id FROM sessions WHERE name = ?1",
                params![name],
                |r| r.get::<_, i64>(0),
            )
            .optional()?
        else {
            return Ok(false);
        };
        // `events` carries no foreign key, so its rows would outlive the
        // session. That is worse than a leak: SQLite hands a freed id out again,
        // and the dead session's events then pose as the new session's history.
        tx.execute("DELETE FROM events WHERE session_id = ?1", params![id])?;
        let n = tx.execute("DELETE FROM sessions WHERE name = ?1", params![name])?;
        tx.commit()?;
        Ok(n > 0)
    }

    pub fn list_sessions(&self) -> anyhow::Result<Vec<SessionRow>> {
        let conn = self.lock();
        let mut stmt = conn.prepare(
            "SELECT s.id, s.name, s.status, s.model, s.cwd, s.created_at, s.updated_at,
                    (SELECT COUNT(*) FROM messages m WHERE m.session_id = s.id) AS messages
             FROM sessions s
             ORDER BY s.updated_at DESC",
        )?;
        let rows = stmt
            .query_map([], |r| {
                Ok(SessionRow {
                    id: r.get(0)?,
                    name: r.get(1)?,
                    status: parse_status(&r.get::<_, String>(2)?),
                    model: r.get(3)?,
                    cwd: r.get(4)?,
                    created_at: r.get(5)?,
                    updated_at: r.get(6)?,
                    messages: r.get::<_, i64>(7)? as u64,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    pub fn get_session(&self, name: &str) -> anyhow::Result<Option<SessionRow>> {
        Ok(self
            .list_sessions()?
            .into_iter()
            .find(|s| s.name == name))
    }

    pub fn touch_session(&self, id: i64) -> anyhow::Result<()> {
        self.lock().execute(
            "UPDATE sessions SET updated_at = ?1 WHERE id = ?2",
            params![now_millis(), id],
        )?;
        Ok(())
    }

    pub fn set_status(&self, id: i64, status: SessionStatus) -> anyhow::Result<()> {
        self.lock().execute(
            "UPDATE sessions SET status = ?1 WHERE id = ?2",
            params![status_name(status), id],
        )?;
        Ok(())
    }

    // -- transcript --------------------------------------------------------

    /// Appends a message and returns its sequence number within the session.
    pub fn append_message(
        &self,
        session_id: i64,
        kind: &str,
        content: &serde_json::Value,
    ) -> anyhow::Result<i64> {
        let conn = self.lock();
        let seq: i64 = conn
            .query_row(
                "SELECT COALESCE(MAX(seq), 0) + 1 FROM messages WHERE session_id = ?1",
                params![session_id],
                |r| r.get(0),
            )
            .unwrap_or(1);
        conn.execute(
            "INSERT INTO messages (session_id, seq, kind, content, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                session_id,
                seq,
                kind,
                serde_json::to_string(content)?,
                now_millis()
            ],
        )?;
        Ok(seq)
    }

    /// Loads the transcript in the order the model should see it.
    pub fn history(&self, session_id: i64) -> anyhow::Result<Vec<HistoryItem>> {
        self.history_since(session_id, 0)
    }

    /// The part of the transcript after `seq`. A summary, once written, stands
    /// in for everything up to its `summarised_upto`, so that is all a request
    /// needs. The rows themselves are never deleted -- the full transcript
    /// stays available to `history` and to anything that wants to audit it.
    pub fn history_since(
        &self,
        session_id: i64,
        after_seq: i64,
    ) -> anyhow::Result<Vec<HistoryItem>> {
        let rows = {
            let conn = self.lock();
            let mut stmt = conn.prepare(
                "SELECT kind, content FROM messages WHERE session_id = ?1 AND seq > ?2
                 ORDER BY seq",
            )?;
            stmt.query_map(params![session_id, after_seq], |r| {
                let kind: String = r.get(0)?;
                let content: String = r.get(1)?;
                Ok((kind, content))
            })?
            .collect::<Result<Vec<_>, _>>()?
        };

        let mut out = Vec::with_capacity(rows.len());
        for (kind, content) in rows {
            let value: serde_json::Value = serde_json::from_str(&content)
                .unwrap_or(serde_json::Value::Null);
            match kind.as_str() {
                "text" => out.push(HistoryItem::Text {
                    role: value
                        .get("role")
                        .and_then(|v| v.as_str())
                        .unwrap_or("user")
                        .to_string(),
                    text: value
                        .get("text")
                        .and_then(|v| v.as_str())
                        .unwrap_or_default()
                        .to_string(),
                }),
                "reasoning" => out.push(HistoryItem::Reasoning {
                    item_id: value.get("item_id").and_then(|v| v.as_str()).map(str::to_string),
                    encrypted: value
                        .get("encrypted")
                        .and_then(|v| v.as_str())
                        .map(str::to_string),
                    summary: value
                        .get("summary")
                        .and_then(|v| v.as_str())
                        .unwrap_or_default()
                        .to_string(),
                }),
                "tool_call" => out.push(HistoryItem::ToolCall {
                    call_id: value
                        .get("call_id")
                        .and_then(|v| v.as_str())
                        .unwrap_or_default()
                        .to_string(),
                    name: value
                        .get("name")
                        .and_then(|v| v.as_str())
                        .unwrap_or_default()
                        .to_string(),
                    arguments: value
                        .get("arguments")
                        .and_then(|v| v.as_str())
                        .unwrap_or_default()
                        .to_string(),
                }),
                "tool_result" => out.push(HistoryItem::ToolResult {
                    call_id: value
                        .get("call_id")
                        .and_then(|v| v.as_str())
                        .unwrap_or_default()
                        .to_string(),
                    output: value
                        .get("output")
                        .and_then(|v| v.as_str())
                        .unwrap_or_default()
                        .to_string(),
                }),
                other => tracing::warn!("unknown message kind {other}"),
            }
        }
        Ok(out)
    }

    /// `(seq, kind)` of every message, oldest first. Compression needs the seqs
    /// to decide where a summary may take over, not just the contents.
    pub fn message_index(&self, session_id: i64) -> anyhow::Result<Vec<(i64, String)>> {
        let conn = self.lock();
        let mut stmt = conn.prepare(
            "SELECT seq, kind FROM messages WHERE session_id = ?1 ORDER BY seq",
        )?;
        let rows = stmt
            .query_map(params![session_id], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// The compressed prefix, if there is one: its text and the seq it covers.
    pub fn summary(&self, session_id: i64) -> anyhow::Result<(Option<String>, i64)> {
        let conn = self.lock();
        let row = conn.query_row(
            "SELECT summary, summarised_upto FROM sessions WHERE id = ?1",
            params![session_id],
            |r| Ok((r.get::<_, Option<String>>(0)?, r.get::<_, i64>(1)?)),
        )?;
        Ok(row)
    }

    /// Records a summary of everything up to and including `upto`.
    pub fn set_summary(
        &self,
        session_id: i64,
        summary: &str,
        upto: i64,
    ) -> anyhow::Result<()> {
        self.lock().execute(
            "UPDATE sessions SET summary = ?1, summarised_upto = ?2 WHERE id = ?3",
            params![summary, upto, session_id],
        )?;
        Ok(())
    }

    /// What a request is built from: the summary, plus whatever happened after
    /// it. Returns `(None, full history)` when nothing has been compressed.
    pub fn context(&self, session_id: i64) -> anyhow::Result<(Option<String>, Vec<HistoryItem>)> {
        let (summary, upto) = self.summary(session_id)?;
        let items = self.history_since(session_id, upto)?;
        Ok((summary, items))
    }

    // -- tool calls --------------------------------------------------------

    pub fn start_tool_call(
        &self,
        session_id: i64,
        call_id: &str,
        name: &str,
        arguments: &str,
    ) -> anyhow::Result<()> {
        self.lock().execute(
            "INSERT INTO tool_calls (session_id, call_id, name, arguments, status, started_at)
             VALUES (?1, ?2, ?3, ?4, 'running', ?5)",
            params![session_id, call_id, name, arguments, now_millis()],
        )?;
        Ok(())
    }

    pub fn finish_tool_call(
        &self,
        session_id: i64,
        call_id: &str,
        output: &str,
        status: &str,
    ) -> anyhow::Result<()> {
        self.lock().execute(
            "UPDATE tool_calls SET output = ?1, status = ?2, finished_at = ?3
             WHERE session_id = ?4 AND call_id = ?5",
            params![output, status, now_millis(), session_id, call_id],
        )?;
        Ok(())
    }

    // -- event log ---------------------------------------------------------

    pub fn append_event(
        &self,
        session_id: Option<i64>,
        kind: &str,
        payload: &serde_json::Value,
    ) -> anyhow::Result<()> {
        let conn = self.lock();
        let seq: i64 = conn
            .query_row("SELECT COALESCE(MAX(seq), 0) + 1 FROM events", [], |r| r.get(0))
            .unwrap_or(1);
        conn.execute(
            "INSERT INTO events (session_id, seq, kind, payload, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                session_id,
                seq,
                kind,
                serde_json::to_string(payload)?,
                now_millis()
            ],
        )?;
        Ok(())
    }

    // -- pending tasks ------------------------------------------------------

    /// Records work that has started but not finished.
    ///
    /// The row is written *before* the work runs and deleted once it completes,
    /// so an aborted process is indistinguishable from a deliberate `cancel`:
    /// either way, whatever is still in this table is what the next start owes.
    pub fn add_pending_task(
        &self,
        session: &str,
        kind: &str,
        payload: &serde_json::Value,
    ) -> anyhow::Result<i64> {
        let now = now_millis();
        let mut conn = self.lock();
        let tx = conn.transaction()?;
        tx.execute(
            "INSERT INTO pending_tasks (session, kind, payload, created_at)
             VALUES (?1, ?2, ?3, ?4)",
            params![session, kind, serde_json::to_string(payload)?, now],
        )?;
        let id = tx.last_insert_rowid();
        tx.commit()?;
        Ok(id)
    }

    /// Everything left undone, oldest first.
    pub fn pending_tasks(&self) -> anyhow::Result<Vec<PendingTask>> {
        let conn = self.lock();
        let mut stmt = conn.prepare(
            "SELECT id, session, kind, payload FROM pending_tasks ORDER BY id",
        )?;
        let rows = stmt
            .query_map([], |r| {
                let payload: String = r.get(3)?;
                Ok(PendingTask {
                    id: r.get(0)?,
                    session: r.get(1)?,
                    kind: r.get(2)?,
                    payload: serde_json::from_str(&payload).unwrap_or(serde_json::Value::Null),
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    /// Drops the pending work of one session, i.e. the turn finished.
    pub fn clear_pending_tasks(&self, session: &str) -> anyhow::Result<usize> {
        Ok(self
            .lock()
            .execute("DELETE FROM pending_tasks WHERE session = ?1", params![session])?)
    }

    pub fn delete_pending_task(&self, id: i64) -> anyhow::Result<()> {
        self.lock()
            .execute("DELETE FROM pending_tasks WHERE id = ?1", params![id])?;
        Ok(())
    }

    // -- detached jobs ------------------------------------------------------

    /// Records a process that outlives this run of the server.
    ///
    /// Written before the job is handed to the model, so a crash in between
    /// leaves a row the next start adopts rather than a process nobody knows
    /// about.
    pub fn add_detached_job(&self, job: &DetachedRow) -> anyhow::Result<i64> {
        let mut conn = self.lock();
        let tx = conn.transaction()?;
        tx.execute(
            "INSERT INTO detached_jobs (session, shell, job, pid, cmd, log, started_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                job.session,
                job.shell,
                job.job as i64,
                job.pid,
                job.cmd,
                job.log,
                job.started_at
            ],
        )?;
        let id = tx.last_insert_rowid();
        tx.commit()?;
        Ok(id)
    }

    /// Every recorded job, oldest first.
    pub fn detached_jobs(&self) -> anyhow::Result<Vec<DetachedRow>> {
        let conn = self.lock();
        let mut stmt = conn.prepare(
            "SELECT id, session, shell, job, pid, cmd, log, started_at
             FROM detached_jobs ORDER BY id",
        )?;
        let rows = stmt
            .query_map([], |r| {
                Ok(DetachedRow {
                    id: r.get(0)?,
                    session: r.get(1)?,
                    shell: r.get(2)?,
                    job: r.get::<_, i64>(3)? as u64,
                    pid: r.get(4)?,
                    cmd: r.get(5)?,
                    log: r.get(6)?,
                    started_at: r.get(7)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    pub fn delete_detached_job(&self, id: i64) -> anyhow::Result<()> {
        self.lock()
            .execute("DELETE FROM detached_jobs WHERE id = ?1", params![id])?;
        Ok(())
    }
}

/// A row of unfinished work, replayed on the next start.
#[derive(Debug, Clone)]
pub struct PendingTask {
    pub id: i64,
    pub session: String,
    pub kind: String,
    pub payload: serde_json::Value,
}

/// A process started with `bash --detached`.
#[derive(Debug, Clone)]
pub struct DetachedRow {
    pub id: i64,
    pub session: String,
    pub shell: String,
    /// Job number, unique within the session.
    pub job: u64,
    pub pid: i64,
    pub cmd: String,
    /// Where its output is written; survives the job and the server.
    pub log: String,
    pub started_at: i64,
}

#[derive(Debug, Clone)]
pub struct SessionRow {
    pub id: i64,
    pub name: String,
    pub status: SessionStatus,
    pub model: String,
    pub cwd: String,
    pub created_at: i64,
    pub updated_at: i64,
    pub messages: u64,
}

/// A transcript entry, already decoded into the shape the LLM adapter wants.
#[derive(Debug, Clone)]
pub enum HistoryItem {
    Text { role: String, text: String },
    Reasoning {
        item_id: Option<String>,
        encrypted: Option<String>,
        summary: String,
    },
    ToolCall {
        call_id: String,
        name: String,
        arguments: String,
    },
    ToolResult {
        call_id: String,
        output: String,
    },
}

pub fn status_name(status: SessionStatus) -> &'static str {
    match status {
        SessionStatus::Idle => "idle",
        SessionStatus::Running => "running",
        SessionStatus::Error => "error",
    }
}

fn parse_status(s: &str) -> SessionStatus {
    match s {
        "running" => SessionStatus::Running,
        "error" => SessionStatus::Error,
        _ => SessionStatus::Idle,
    }
}

fn duplicate_session(name: &str, e: rusqlite::Error) -> anyhow::Error {
    if matches!(e, rusqlite::Error::SqliteFailure(ref f, _) if f.code == rusqlite::ErrorCode::ConstraintViolation)
    {
        return anyhow::anyhow!("session {name:?} already exists");
    }
    anyhow::Error::from(e)
}

pub fn now_millis() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn temp_store() -> (tempfile::TempDir, Store) {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(&dir.path().join("g.db")).unwrap();
        (dir, store)
    }

    /// Two messages queued on one session used to share a fate: the first turn
    /// cleared the session's pending rows wholesale and took the second
    /// message's row with it, so a crash before the second turn ran lost the
    /// message from both places at once.
    #[test]
    fn finishing_one_pending_task_leaves_the_others() {
        let (_d, store) = temp_store();
        store.create_session("main", "m", "C:/x").unwrap();
        let first = store
            .add_pending_task("main", "turn", &json!({"message": "one"}))
            .unwrap();
        let second = store
            .add_pending_task("main", "turn", &json!({"message": "two"}))
            .unwrap();

        store.delete_pending_task(first).unwrap();
        let left = store.pending_tasks().unwrap();
        assert_eq!(left.len(), 1, "only the finished turn's row goes");
        assert_eq!(left[0].id, second);
    }

    #[test]
    fn sessions_round_trip() {
        let (_d, store) = temp_store();
        store.create_session("main", "m", "C:/x").unwrap();
        let list = store.list_sessions().unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].name, "main");
        assert!(store.delete_session("main").unwrap());
        assert!(store.list_sessions().unwrap().is_empty());
    }

    #[test]
    fn duplicate_session_names_are_rejected() {
        let (_d, store) = temp_store();
        store.create_session("a", "m", "C:/x").unwrap();
        let err = store.create_session("a", "m", "C:/x").unwrap_err();
        assert!(err.to_string().contains("already exists"), "{err}");
    }

    #[test]
    fn history_preserves_order_and_types() {
        let (_d, store) = temp_store();
        let id = store.create_session("s", "m", "C:/x").unwrap();
        store
            .append_message(id, "text", &serde_json::json!({"role":"user","text":"hi"}))
            .unwrap();
        store
            .append_message(
                id,
                "tool_call",
                &serde_json::json!({"call_id":"c1","name":"bash","arguments":"{}"}),
            )
            .unwrap();
        store
            .append_message(
                id,
                "tool_result",
                &serde_json::json!({"call_id":"c1","output":"ok"}),
            )
            .unwrap();
        let h = store.history(id).unwrap();
        assert_eq!(h.len(), 3);
        assert!(matches!(h[0], HistoryItem::Text { .. }));
        assert!(matches!(h[1], HistoryItem::ToolCall { .. }));
        assert!(matches!(h[2], HistoryItem::ToolResult { .. }));
    }

    /// A summary stands in for the messages it covers, but the rows stay: the
    /// transcript is a record, not a cache, and deleting them would lose the
    /// only copy of what the session actually did.
    #[test]
    fn a_summary_shrinks_the_request_without_touching_the_transcript() {
        let (_d, store) = temp_store();
        let id = store.create_session("s", "m", "C:/x").unwrap();
        for i in 0..5 {
            store
                .append_message(id, "text", &serde_json::json!({"role":"user","text":format!("m{i}")}))
                .unwrap();
        }
        assert_eq!(store.summary(id).unwrap(), (None, 0));

        store.set_summary(id, "the first three", 3).unwrap();

        let (summary, items) = store.context(id).unwrap();
        assert_eq!(summary.as_deref(), Some("the first three"));
        assert_eq!(items.len(), 2, "only what came after the summary is sent");
        // The full transcript is still there.
        assert_eq!(store.history(id).unwrap().len(), 5);
        assert_eq!(store.message_index(id).unwrap().len(), 5);
    }

    /// A database written before compression existed has to grow the columns
    /// rather than fail to open, or every existing session would be stranded.
    #[test]
    fn opening_an_old_database_adds_the_summary_columns() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("g.db");
        {
            let conn = rusqlite::Connection::open(&path).unwrap();
            conn.execute_batch(
                "CREATE TABLE sessions (id INTEGER PRIMARY KEY, name TEXT NOT NULL UNIQUE, \
                 status TEXT NOT NULL, model TEXT NOT NULL, cwd TEXT NOT NULL, \
                 created_at INTEGER NOT NULL, updated_at INTEGER NOT NULL);",
            )
            .unwrap();
        }
        let store = Store::open(&path).unwrap();
        assert_eq!(store.summary(1).unwrap_or((None, 0)), (None, 0));
    }

    #[test]
    fn detached_jobs_survive_until_stopped() {
        let (_d, store) = temp_store();
        let id = store
            .add_detached_job(&DetachedRow {
                id: 0,
                session: "main".into(),
                shell: "default".into(),
                job: 1,
                pid: 4242,
                cmd: "gongga-server --bind 127.0.0.1:9800".into(),
                log: "C:/tmp/main-1.log".into(),
                started_at: 7,
            })
            .unwrap();
        let rows = store.detached_jobs().unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].job, 1);
        assert_eq!(rows[0].pid, 4242);
        assert_eq!(rows[0].log, "C:/tmp/main-1.log");
        store.delete_detached_job(id).unwrap();
        assert!(store.detached_jobs().unwrap().is_empty());
    }

    #[test]
    fn pending_tasks_survive_until_cleared() {
        let (_d, store) = temp_store();
        store
            .add_pending_task("main", "turn", &serde_json::json!({"message": "hi"}))
            .unwrap();
        let tasks = store.pending_tasks().unwrap();
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].session, "main");
        assert_eq!(tasks[0].payload["message"], "hi");
        // A finished turn is no longer owed, so it goes away.
        assert_eq!(store.clear_pending_tasks("main").unwrap(), 1);
        assert!(store.pending_tasks().unwrap().is_empty());
    }

    #[test]
    fn deleting_a_session_cascades() {
        let (_d, store) = temp_store();
        let id = store.create_session("s", "m", "C:/x").unwrap();
        store
            .append_message(id, "text", &serde_json::json!({"role":"user","text":"hi"}))
            .unwrap();
        store.delete_session("s").unwrap();
        assert!(store.history(id).unwrap().is_empty());
    }

    /// Events have no foreign key to rely on, so `delete_session` has to take
    /// them explicitly. Otherwise a deleted session's rows stay behind and, once
    /// SQLite reuses the id, show up as the next session's history.
    #[test]
    fn deleting_a_session_takes_its_events_with_it() {
        let (_d, store) = temp_store();
        let id = store.create_session("s", "m", "C:/x").unwrap();
        store
            .append_event(Some(id), "tool_result", &serde_json::json!({"ok": true}))
            .unwrap();
        assert!(store.delete_session("s").unwrap());
        let n: i64 = store
            .lock()
            .query_row("SELECT COUNT(*) FROM events WHERE session_id = ?1", params![id], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(n, 0);
    }
}
