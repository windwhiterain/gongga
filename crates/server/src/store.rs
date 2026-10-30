//! Everything durable lives in SQLite.
//! See `docs/notes/server-store.md`.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};

use protocol::SessionStatus;
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use serde_json::json;

/// The prose one stretch of requests was built from, stored when it was rendered.
///
/// # Invariant
///
/// What the model was told is in the database rather than a rendering to re-guess:
/// a step sends this row, and `#[serde(default)]` keeps an older row readable.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Prompt {
    /// The system instructions this session was sent under, verbatim.
    #[serde(default)]
    pub instructions: String,
    /// The handover note as the model reads it; nothing before the first fold.
    #[serde(default)]
    pub handover: Option<String>,
    /// The directory the shell stood in, so a move is noticed.
    #[serde(default)]
    pub cwd: String,
    #[serde(default)]
    pub stamp: String,
}

/// What the note is, so a fold shows up without rendering prose.
pub fn stamp(note: Option<&str>) -> String {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    note.hash(&mut h);
    format!("{:x}", h.finish())
}

/// What the model is told about a call that never returned: the
/// `interrupted_call` template.
fn interrupted_call() -> String {
    crate::prompts::shared()
        .text("interrupted_call")
        .unwrap_or_default()
}

const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS sessions (
    id         INTEGER PRIMARY KEY,
    name       TEXT NOT NULL UNIQUE,
    status     TEXT NOT NULL DEFAULT 'idle',
    model      TEXT NOT NULL,
    cwd        TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL,
    -- Set once the agent has handed the transcript to a mentor and woken up to
    -- this note: a row is either here or in the mentor, so there is no boundary
    -- to record. Nothing here rewrites a row. The rows can still be
    -- deleted -- by the agent, from a shell, through the `heir` skill, and only
    -- after the whole transcript has been copied into a mentor session and the
    -- copy checked row for row. The server's own hands stay off the transcript.
    handover TEXT,
    -- Per-session overrides given by `set_session`. NULL means "the server's
    -- configuration", so a session that never asked is indistinguishable from
    -- one created before these columns existed.
    llm TEXT,
    context_limit INTEGER,
    -- How hard the model should think, in the roster's spelling. NULL means the
    -- model's own default, which is what a session that never asked should get.
    thinking_effort TEXT,
    -- Input tokens the provider reported for the last request this session
    -- made. Not an estimate: it is the number the provider billed, which is the
    -- only honest answer to "how full is the context". Surviving a restart is
    -- the point -- the first request of the next run needs a size to compare
    -- against, and no amount of character counting would give a better one.
    context_tokens INTEGER NOT NULL DEFAULT 0
);

CREATE TABLE IF NOT EXISTS messages (
    id         INTEGER PRIMARY KEY,
    session_id INTEGER NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
    seq        INTEGER NOT NULL,
    kind       TEXT NOT NULL,
    -- The entry as the LLM API takes it: one standard Responses input item, sent verbatim.
    -- NULL is the marker for a row no request is built from -- a notice, the prompt in force,
    -- an item whose binary payload was gone before it could be replayed.
    content    TEXT,
    -- What the reader needs: the payload as it was written, never sent.
    display    TEXT NOT NULL,
    -- The tool this row calls or answers, so a request can pair them without
    -- opening the item's bytes. NULL for every other kind of row.
    call_id    TEXT,
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
pub struct Store {
    conn: Arc<Mutex<Connection>>,
    /// Where the database is, so a request can open its own read-only view of it.
    path: PathBuf,
}

/// Whether `table` already has `column`, and adds it when it does not.
fn add_column_if_missing(
    conn: &Connection,
    table: &str,
    column: &str,
    ddl: &str,
) -> rusqlite::Result<()> {
    if !has_column(conn, table, column) {
        conn.execute(ddl, [])?;
    }
    Ok(())
}

/// Whether `table` already answers a select on `column`.
fn has_column(conn: &Connection, table: &str, column: &str) -> bool {
    conn.prepare(&format!("SELECT {column} FROM {table} LIMIT 1"))
        .is_ok()
}

/// Adds columns a `CREATE TABLE IF NOT EXISTS` cannot add to an old database.
fn migrate(conn: &Connection) -> rusqlite::Result<()> {
    for (column, ddl) in [
        ("handover", "ALTER TABLE sessions ADD COLUMN handover TEXT"),
        ("llm", "ALTER TABLE sessions ADD COLUMN llm TEXT"),
        (
            "context_limit",
            "ALTER TABLE sessions ADD COLUMN context_limit INTEGER",
        ),
        (
            "context_tokens",
            "ALTER TABLE sessions ADD COLUMN context_tokens INTEGER NOT NULL DEFAULT 0",
        ),
        (
            "thinking_effort",
            "ALTER TABLE sessions ADD COLUMN thinking_effort TEXT",
        ),
    ] {
        add_column_if_missing(conn, "sessions", column, ddl)?;
    }
    // An old row needs a name before it can be listed or deleted.
    add_column_if_missing(
        conn,
        "detached_jobs",
        "name",
        "ALTER TABLE detached_jobs ADD COLUMN name TEXT NOT NULL DEFAULT ''",
    )?;
    conn.execute(
        "UPDATE detached_jobs SET name = shell || '-' || job WHERE name = ''",
        [],
    )?;
    add_column_if_missing(
        conn,
        "messages",
        "call_id",
        "ALTER TABLE messages ADD COLUMN call_id TEXT",
    )?;
    backfill_wire_items(conn)?;
    Ok(())
}

/// Gives the rows a version-1 home wrote the item their requests will send.
///
/// # Invariant
///
/// The split itself is the version step, which renames the column before the database
/// opens; this applies the writer's own rule to the rows that predate it, so what a row
/// sends has one spelling. A row with no `content` was never split, which makes this a
/// no-op on a home that has been through it.
fn backfill_wire_items(conn: &rusqlite::Connection) -> rusqlite::Result<()> {
    if !has_column(conn, "messages", "content") || !has_column(conn, "messages", "display") {
        return Ok(());
    }
    let rows: Vec<(i64, String)> = conn
        .prepare("SELECT id, kind FROM messages WHERE content IS NULL ORDER BY id")?
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<Result<_, _>>()?;
    for (id, kind) in rows {
        let display: String = conn.query_row(
            "SELECT display FROM messages WHERE id = ?1",
            params![id],
            |r| r.get(0),
        )?;
        let value = serde_json::from_str(&display).unwrap_or(serde_json::Value::Null);
        conn.execute(
            "UPDATE messages SET content = ?1, call_id = ?2 WHERE id = ?3",
            params![
                wire_item(&kind, &value).map(|item| item.to_string()),
                value.get("call_id").and_then(|v| v.as_str()),
                id
            ],
        )?;
    }
    Ok(())
}

impl Store {
    /// Opens (and creates) the database at `path`, taking `:memory:` literally.
    pub fn open(path: &Path) -> anyhow::Result<Self> {
        if path != Path::new(":memory:") {
            if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
                std::fs::create_dir_all(parent)?;
            }
        }
        let conn = Connection::open(path)?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        conn.execute_batch(SCHEMA)?;
        migrate(&conn)?;
        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
            path: path.to_path_buf(),
        })
    }

    fn lock(&self) -> MutexGuard<'_, Connection> {
        // A poisoned mutex is safe to recover: the WAL keeps the database consistent.
        self.conn.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Where the database lives, for a reader that brings its own connection.
    pub fn path(&self) -> &Path {
        &self.path
    }

    // -- sessions ----------------------------------------------------------

    pub fn create_session(&self, name: &str, model: &str, cwd: &str) -> anyhow::Result<i64> {
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
        // These tables have no session foreign key; none may leak into a reused name.
        tx.execute("DELETE FROM events WHERE session_id = ?1", params![id])?;
        tx.execute(
            "DELETE FROM pending_tasks WHERE session = ?1",
            params![name],
        )?;
        let n = tx.execute("DELETE FROM sessions WHERE name = ?1", params![name])?;
        tx.commit()?;
        Ok(n > 0)
    }

    /// The provider payloads that only this session's rows refer to, so deleting
    /// the session can delete their files too.
    ///
    /// # Invariant
    ///
    /// Reachability is decided here, in SQL, over every row: a file another session
    /// still names -- an `heir` copy, a second session that asked for the same item --
    /// has to stay. Ask before the rows go, because after them there is nothing left
    /// to ask about.
    pub fn orphaned_blobs(&self, name: &str) -> anyhow::Result<Vec<String>> {
        let conn = self.lock();
        let mut stmt = conn.prepare(
            "SELECT DISTINCT json_extract(display, '$.item.result_blob') FROM messages
              WHERE session_id = (SELECT id FROM sessions WHERE name = ?1)
                AND kind = 'builtin_call'
                AND json_extract(display, '$.item.result_blob') IS NOT NULL
                AND json_extract(display, '$.item.result_blob') NOT IN (
                    SELECT json_extract(display, '$.item.result_blob') FROM messages
                     WHERE session_id != (SELECT id FROM sessions WHERE name = ?1)
                       AND json_extract(display, '$.item.result_blob') IS NOT NULL)",
        )?;
        let ids = stmt
            .query_map(params![name], |r| r.get::<_, String>(0))?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(ids)
    }

    pub fn list_sessions(&self) -> anyhow::Result<Vec<SessionRow>> {
        let conn = self.lock();
        let mut stmt = conn.prepare(&format!(
            "{} ORDER BY s.updated_at DESC",
            Self::SESSION_SELECT
        ))?;
        let rows = stmt
            .query_map([], Self::session_row)?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// The columns every session lookup returns, so the list and the single
    /// lookup cannot drift apart.
    const SESSION_SELECT: &'static str = "SELECT s.id, s.name, s.status, s.model, s.cwd,
                    s.created_at, s.updated_at,
                    (SELECT COUNT(*) FROM messages m WHERE m.session_id = s.id) AS messages,
                    s.llm, s.context_limit, s.thinking_effort
             FROM sessions s";

    /// One session, in the same shape the list returns, by name rather than by scan.
    pub fn get_session(&self, name: &str) -> anyhow::Result<Option<SessionRow>> {
        let conn = self.lock();
        let mut stmt = conn.prepare(&format!("{} WHERE s.name = ?1", Self::SESSION_SELECT))?;
        let row = stmt.query_row([name], Self::session_row).optional()?;
        Ok(row)
    }

    fn session_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<SessionRow> {
        Ok(SessionRow {
            id: r.get(0)?,
            name: r.get(1)?,
            status: parse_status(&r.get::<_, String>(2)?),
            model: r.get(3)?,
            cwd: r.get(4)?,
            created_at: r.get(5)?,
            updated_at: r.get(6)?,
            messages: r.get::<_, i64>(7)? as u64,
            llm: r.get(8)?,
            context_limit: r.get::<_, Option<i64>>(9)?.map(|v| v as u8),
            thinking_effort: r.get(10)?,
        })
    }

    /// Records what a session was told to use: tuning columns and `model` at once.
    ///
    /// # Invariant
    ///
    /// All of it in one statement, so no reader sees half-applied tuning.
    pub fn set_session_tuning(
        &self,
        id: i64,
        llm: Option<&str>,
        context_limit: Option<u8>,
        thinking_effort: Option<&str>,
        model: &str,
    ) -> anyhow::Result<()> {
        self.lock().execute(
            "UPDATE sessions
                SET llm = ?1,
                    context_limit = ?2,
                    thinking_effort = ?3,
                    model = ?4,
                    updated_at = ?5
              WHERE id = ?6",
            params![
                llm,
                context_limit.map(|v| v as i64),
                thinking_effort,
                model,
                now_millis(),
                id
            ],
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
        Self::append_message_conn(&self.lock(), session_id, kind, content)
    }

    fn append_message_conn(
        conn: &rusqlite::Connection,
        session_id: i64,
        kind: &str,
        content: &serde_json::Value,
    ) -> anyhow::Result<i64> {
        let seq: i64 = conn.query_row(
            "INSERT INTO messages (session_id, seq, kind, content, display, call_id, created_at)
             VALUES (
                ?1,
                (SELECT COALESCE(MAX(seq), 0) + 1 FROM messages WHERE session_id = ?1),
                ?2,
                ?3,
                ?4,
                ?5,
                ?6
             )
             RETURNING seq",
            params![
                session_id,
                kind,
                wire_item(kind, content).map(|item| item.to_string()),
                serde_json::to_string(content)?,
                content.get("call_id").and_then(|v| v.as_str()),
                now_millis()
            ],
            |r| r.get(0),
        )?;
        Ok(seq)
    }

    /// The whole transcript, as a request would see it. Test-only: nothing else needs it.
    #[cfg(test)]
    pub fn history(&self, session_id: i64) -> anyhow::Result<Vec<HistoryItem>> {
        self.history_between(session_id, 0, i64::MAX)
    }

    pub fn history_since(
        &self,
        session_id: i64,
        after_seq: i64,
    ) -> anyhow::Result<Vec<HistoryItem>> {
        self.history_between(session_id, after_seq, i64::MAX)
    }

    /// The rows in `(after_seq, upto_seq]`, mapped exactly like [`Self::history_since`].
    pub fn history_between(
        &self,
        session_id: i64,
        after_seq: i64,
        upto_seq: i64,
    ) -> anyhow::Result<Vec<HistoryItem>> {
        let rows = {
            let conn = self.lock();
            let mut stmt = conn.prepare(
                "SELECT kind, display FROM messages
                  WHERE session_id = ?1 AND seq > ?2 AND seq <= ?3
                  ORDER BY seq",
            )?;
            stmt.query_map(params![session_id, after_seq, upto_seq], |r| {
                let kind: String = r.get(0)?;
                let content: String = r.get(1)?;
                Ok((kind, content))
            })?
            .collect::<Result<Vec<_>, _>>()?
        };

        let mut out = Vec::with_capacity(rows.len());
        // An unanswered call gets a placeholder; providers reject a bare `function_call`.
        let answered: std::collections::HashSet<String> = rows
            .iter()
            .filter(|(kind, _)| kind == "tool_result")
            .filter_map(|(_, content)| {
                serde_json::from_str::<serde_json::Value>(content)
                    .ok()
                    .and_then(|v| {
                        v.get("call_id")
                            .and_then(|c| c.as_str())
                            .map(str::to_string)
                    })
            })
            .collect();
        // The same rule backwards: a result whose call is not in this window is a
        // lone output, and the provider rejects that.
        let called: std::collections::HashSet<String> = rows
            .iter()
            .filter(|(kind, _)| kind == "tool_call")
            .filter_map(|(_, content)| {
                serde_json::from_str::<serde_json::Value>(content)
                    .ok()
                    .and_then(|v| {
                        v.get("call_id")
                            .and_then(|c| c.as_str())
                            .map(str::to_string)
                    })
            })
            .collect();
        for (kind, content) in rows {
            let value: serde_json::Value =
                serde_json::from_str(&content).unwrap_or(serde_json::Value::Null);
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
                    item_id: value
                        .get("item_id")
                        .and_then(|v| v.as_str())
                        .map(str::to_string),
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
                "tool_call" => {
                    let call_id = value
                        .get("call_id")
                        .and_then(|v| v.as_str())
                        .unwrap_or_default()
                        .to_string();
                    out.push(HistoryItem::ToolCall {
                        call_id: call_id.clone(),
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
                    });
                    if !answered.contains(&call_id) {
                        out.push(HistoryItem::ToolResult {
                            call_id,
                            output: interrupted_call().into(),
                            images: Vec::new(),
                        });
                    }
                }
                "tool_result" => {
                    let call_id = value
                        .get("call_id")
                        .and_then(|v| v.as_str())
                        .unwrap_or_default()
                        .to_string();
                    if called.contains(&call_id) {
                        out.push(HistoryItem::ToolResult {
                            call_id,
                            output: value
                                .get("output")
                                .and_then(|v| v.as_str())
                                .unwrap_or_default()
                                .to_string(),
                            images: images_of(&value),
                        });
                    } else {
                        tracing::debug!(call_id, "a tool result lost its call: not sent");
                    }
                }
                "builtin_call" => {
                    let item = value
                        .get("item")
                        .cloned()
                        .unwrap_or(serde_json::Value::Null);
                    if !item.is_null() {
                        out.push(HistoryItem::BuiltinCall {
                            item,
                            replayable: value
                                .get("replayable")
                                .and_then(serde_json::Value::as_bool)
                                .unwrap_or(true),
                        });
                    }
                }
                // Operator-only: feeding a notice back would put our bookkeeping in the prompt.
                "notice" => {}
                // This *is* the prompt: sending it back as history would double it.
                "system" => {}
                other => tracing::warn!("unknown message kind {other}"),
            }
        }
        Ok(pair_tool_results(out))
    }

    /// Where the transcript now ends, `0` for an empty one.
    pub fn last_seq(&self, session_id: i64) -> anyhow::Result<i64> {
        let conn = self.lock();
        Ok(conn.query_row(
            "SELECT COALESCE(MAX(seq), 0) FROM messages WHERE session_id = ?1",
            params![session_id],
            |r| r.get(0),
        )?)
    }

    /// Rows in an inclusive `seq` window, oldest first. See `docs/notes/server-store.md`.
    pub fn messages_between(
        &self,
        session_id: i64,
        first: i64,
        last: i64,
    ) -> anyhow::Result<Transcript> {
        let conn = self.lock();
        // One statement: rows and status from the same snapshot. Both
        // sub-selects coalesce so a missing id reads empty and idle.
        let (last_seq, status): (i64, SessionStatus) = conn.query_row(
            "SELECT COALESCE((SELECT MAX(seq) FROM messages WHERE session_id = ?1), 0),
                    COALESCE((SELECT status FROM sessions WHERE id = ?1), 'idle')",
            params![session_id],
            |r| Ok((r.get(0)?, parse_status(&r.get::<_, String>(1)?))),
        )?;
        let mut stmt = conn.prepare(
            "SELECT seq, kind, display FROM messages
             WHERE session_id = ?1 AND seq >= ?2 AND seq <= ?3
             ORDER BY seq ASC",
        )?;
        let rows = stmt
            .query_map(params![session_id, first, last], |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        let messages = rows
            .into_iter()
            .map(|(seq, kind, content)| MessageRow::parse(seq, &kind, &content))
            .collect();
        Ok(Transcript {
            last_seq,
            status,
            messages,
        })
    }

    /// `(seq, kind)` of every message, oldest first. Test-only: folding left the
    /// server.
    #[cfg(test)]
    pub fn message_index(&self, session_id: i64) -> anyhow::Result<Vec<(i64, String)>> {
        let conn = self.lock();
        let mut stmt =
            conn.prepare("SELECT seq, kind FROM messages WHERE session_id = ?1 ORDER BY seq")?;
        let rows = stmt
            .query_map(params![session_id], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// The handover note, if the agent has written one.
    pub fn handover(&self, session_id: i64) -> anyhow::Result<Option<String>> {
        let conn = self.lock();
        let note = conn.query_row(
            "SELECT handover FROM sessions WHERE id = ?1",
            params![session_id],
            |r| r.get::<_, Option<String>>(0),
        )?;
        Ok(note)
    }

    /// Input tokens the provider reported for the last request, or 0 if this
    /// session has not made one yet.
    pub fn context_tokens(&self, session_id: i64) -> anyhow::Result<u64> {
        let conn = self.lock();
        let n: i64 = conn.query_row(
            "SELECT context_tokens FROM sessions WHERE id = ?1",
            params![session_id],
            |r| r.get(0),
        )?;
        Ok(n.max(0) as u64)
    }

    /// Records what the provider said the last request cost in input tokens.
    pub fn set_context_tokens(&self, session_id: i64, tokens: u64) -> anyhow::Result<()> {
        self.lock().execute(
            "UPDATE sessions SET context_tokens = ?1 WHERE id = ?2",
            params![tokens as i64, session_id],
        )?;
        Ok(())
    }

    /// The newest stored prompt, or nothing before a session's first request.
    pub fn prompt(&self, session_id: i64) -> anyhow::Result<Option<Prompt>> {
        let row = {
            let conn = self.lock();
            conn.query_row(
                "SELECT display FROM messages \
                  WHERE session_id = ?1 AND kind = 'system' ORDER BY seq DESC LIMIT 1",
                params![session_id],
                |r| r.get::<_, String>(0),
            )
            .optional()?
        };
        let Some(content) = row else { return Ok(None) };
        Ok(serde_json::from_str::<Prompt>(&content).ok())
    }

    /// Stores the prompt the next request is built from, bytes and all.
    pub fn append_prompt(&self, session_id: i64, prompt: &Prompt) -> anyhow::Result<i64> {
        let payload = serde_json::to_value(prompt)?;
        self.append_message(session_id, "system", &payload)
    }

    /// Closes every call that never got a result, and returns how many.
    ///
    /// # Invariant
    ///
    /// A read path must not invent an item the database does not hold: providers
    /// reject the call without its result, so the closing row has to exist, and
    /// writing it here is what makes the next request's array the stored rows and
    /// not a rendering of a gap.
    pub fn close_dangling_calls(&self, session_id: i64) -> anyhow::Result<usize> {
        // Raw rows: history closes an unanswered call itself, and that
        // placeholder must not hide the gap from this write.
        let rows = {
            let conn = self.lock();
            let mut stmt = conn
                .prepare("SELECT kind, display FROM messages WHERE session_id = ?1 ORDER BY seq")?;
            stmt.query_map(params![session_id], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
            })?
            .collect::<Result<Vec<_>, _>>()?
        };
        let call_id = |content: &str| {
            serde_json::from_str::<serde_json::Value>(content)
                .ok()
                .and_then(|v| v.get("call_id")?.as_str().map(str::to_string))
        };
        let mut open: Vec<String> = Vec::new();
        let mut answered: std::collections::HashSet<String> = std::collections::HashSet::new();
        for (kind, content) in &rows {
            let Some(id) = call_id(content) else { continue };
            match kind.as_str() {
                "tool_call" => open.push(id),
                "tool_result" => {
                    answered.insert(id);
                }
                _ => {}
            }
        }
        let text = interrupted_call();
        let mut closed = 0;
        for call_id in open.iter().filter(|id| !answered.contains(*id)) {
            self.append_message(
                session_id,
                "tool_result",
                &json!({"call_id": call_id, "output": text, "images": []}),
            )?;
            closed += 1;
        }
        Ok(closed)
    }

    /// What a request is built from: the handover note plus every row still here.
    ///
    /// A handover leaves the note and nothing else -- the rows that explained it
    /// are in the mentor -- so there is no boundary to cut at.
    pub fn context(&self, session_id: i64) -> anyhow::Result<(Option<String>, Vec<HistoryItem>)> {
        let note = self.handover(session_id)?;
        let items = self.history_since(session_id, 0)?;
        Ok((note, items))
    }

    /// The transcript a request reads, closed at the row that exists right now.
    pub fn request_source(&self, session_id: i64) -> anyhow::Result<crate::llm::RequestSource> {
        Ok(crate::llm::RequestSource {
            db_path: self.path.clone(),
            session_id,
            upto_seq: self.last_seq(session_id)?,
            placeholder: interrupted_call(),
        })
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
            .query_row("SELECT COALESCE(MAX(seq), 0) + 1 FROM events", [], |r| {
                r.get(0)
            })
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

    /// Records work that has started but not finished, before it runs.
    ///
    /// # Invariant
    ///
    /// The row is written before the work runs and deleted once it completes, so
    /// the table always holds what the next start owes.
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

    /// Accepts an idle session's first message and its turn obligation together.
    pub fn begin_turn(&self, session_id: i64, session: &str, message: &str) -> anyhow::Result<i64> {
        let mut conn = self.lock();
        let tx = conn.transaction()?;
        tx.execute(
            "INSERT INTO pending_tasks (session, kind, payload, created_at)
             VALUES (?1, 'turn', ?2, ?3)",
            params![
                session,
                serde_json::to_string(&json!({"session": session}))?,
                now_millis()
            ],
        )?;
        let id = tx.last_insert_rowid();
        Self::append_message_conn(
            &tx,
            session_id,
            "text",
            &json!({"role": "user", "text": message}),
        )?;
        tx.commit()?;
        Ok(id)
    }

    /// Delivers every injected message and removes its inbox row in the same transaction.
    pub fn deliver_pending(&self, session_id: i64, session: &str) -> anyhow::Result<usize> {
        let mut conn = self.lock();
        let tx = conn.transaction()?;
        let tasks = {
            let mut stmt = tx.prepare(
                "SELECT id, payload FROM pending_tasks
                 WHERE session = ?1 AND kind = 'inject' ORDER BY id",
            )?;
            let rows = stmt.query_map(params![session], |row| {
                Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
            })?;
            rows.collect::<Result<Vec<_>, _>>()?
        };
        let delivered = tasks.len();
        for (id, payload) in tasks {
            let payload: serde_json::Value = serde_json::from_str(&payload)?;
            let message = payload
                .get("message")
                .and_then(serde_json::Value::as_str)
                .ok_or_else(|| anyhow::anyhow!("pending inject {id} has no string message"))?;
            Self::append_message_conn(
                &tx,
                session_id,
                "text",
                &json!({"role": "user", "text": message}),
            )?;
            tx.execute("DELETE FROM pending_tasks WHERE id = ?1", params![id])?;
        }
        tx.commit()?;
        Ok(delivered)
    }

    /// Commits the terminal status and the fate of this turn's durable obligations together.
    pub fn finish_turn(
        &self,
        session_id: i64,
        session: &str,
        tasks: &[i64],
        status: SessionStatus,
        preserve: bool,
    ) -> anyhow::Result<()> {
        let mut conn = self.lock();
        let tx = conn.transaction()?;
        tx.execute(
            "UPDATE sessions SET status = ?1, updated_at = ?2 WHERE id = ?3",
            params![status_name(status), now_millis(), session_id],
        )?;
        if !preserve {
            for id in tasks {
                tx.execute(
                    "DELETE FROM pending_tasks WHERE id = ?1 AND session = ?2",
                    params![id, session],
                )?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    /// Everything left undone, oldest first.
    pub fn pending_tasks(&self) -> anyhow::Result<Vec<PendingTask>> {
        let conn = self.lock();
        let mut stmt = conn.prepare(
            "SELECT id, session, kind, payload, created_at FROM pending_tasks ORDER BY id",
        )?;
        let rows = stmt
            .query_map([], |r| {
                let payload: String = r.get(3)?;
                Ok(PendingTask {
                    id: r.get(0)?,
                    session: r.get(1)?,
                    kind: r.get(2)?,
                    payload: serde_json::from_str(&payload).unwrap_or(serde_json::Value::Null),
                    created_at: r.get(4)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    /// Drops the pending work of one session, i.e. the turn finished.
    pub fn clear_pending_tasks(&self, session: &str) -> anyhow::Result<usize> {
        Ok(self.lock().execute(
            "DELETE FROM pending_tasks WHERE session = ?1",
            params![session],
        )?)
    }

    pub fn delete_pending_task(&self, id: i64) -> anyhow::Result<()> {
        self.lock()
            .execute("DELETE FROM pending_tasks WHERE id = ?1", params![id])?;
        Ok(())
    }

    // -- detached jobs ------------------------------------------------------

    /// Records a process that outlives this run, before the model is told of it.
    pub fn add_detached_job(&self, job: &DetachedRow) -> anyhow::Result<i64> {
        let mut conn = self.lock();
        let tx = conn.transaction()?;
        tx.execute(
            "INSERT INTO detached_jobs (session, name, shell, job, pid, cmd, log, started_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                job.session,
                job.name,
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
            "SELECT id, session, name, shell, job, pid, cmd, log, started_at
             FROM detached_jobs ORDER BY id",
        )?;
        let rows = stmt
            .query_map([], |r| {
                Ok(DetachedRow {
                    id: r.get(0)?,
                    session: r.get(1)?,
                    name: r.get(2)?,
                    shell: r.get(3)?,
                    job: r.get::<_, i64>(4)? as u64,
                    pid: r.get(5)?,
                    cmd: r.get(6)?,
                    log: r.get(7)?,
                    started_at: r.get(8)?,
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
    /// When the row was written: how long it has been owed.
    pub created_at: i64,
}

/// A process started with `bash --detached`.
#[derive(Debug, Clone)]
pub struct DetachedRow {
    pub id: i64,
    pub session: String,
    /// The name this detached bash is addressed by, unique within the session.
    pub name: String,
    /// Which shell's directory it started in; a label, not a handle.
    pub shell: String,
    /// Row order within the session; names the log file, never shown to the model.
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
    /// Roster entry this session talks to, when `set_session --llm` gave it one.
    pub llm: Option<String>,
    /// The context level a notice is sent at, as a share of the window, when the
    /// session was given one.
    pub context_limit: Option<u8>,
    /// Reasoning level in the roster's spelling, when the session was given
    /// one. `None` means the model's own default.
    pub thinking_effort: Option<String>,
    pub cwd: String,
    pub created_at: i64,
    pub updated_at: i64,
    pub messages: u64,
}

/// A window of the transcript plus where it ends, as `get_messages` reports it.
///
/// # Invariant
///
/// `status` and `last_seq` come from the same snapshot: a reader is not told
/// about a session that moved on between the rows and the status. It is read in the same statement as `last_seq`, so it is the session's
/// state at the moment the window was taken, not a later guess.
#[derive(Debug, Clone)]
pub struct Transcript {
    pub last_seq: i64,
    pub status: SessionStatus,
    pub messages: Vec<MessageRow>,
}

/// One row of the transcript, flattened for readers that are not the model.
#[derive(Debug, Clone, serde::Serialize)]
pub struct MessageRow {
    pub seq: i64,
    pub kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub call_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    pub text: String,
}

impl MessageRow {
    /// Flatten one stored row, for callers with no store at hand.
    pub(crate) fn parse(seq: i64, kind: &str, content: &str) -> Self {
        let value: serde_json::Value =
            serde_json::from_str(content).unwrap_or(serde_json::Value::Null);
        let str_of = |key: &str| value.get(key).and_then(|v| v.as_str()).map(str::to_string);
        let mut row = MessageRow {
            seq,
            kind: kind.to_string(),
            role: None,
            call_id: None,
            name: None,
            text: String::new(),
        };
        match kind {
            "text" => {
                row.role = str_of("role");
                row.text = str_of("text").unwrap_or_default();
            }
            "reasoning" => row.text = str_of("summary").unwrap_or_default(),
            "tool_call" => {
                row.call_id = str_of("call_id");
                row.name = str_of("name");
                row.text = str_of("arguments").unwrap_or_default();
            }
            "tool_result" => {
                row.call_id = str_of("call_id");
                row.text = str_of("output").unwrap_or_default();
            }
            // The harness talking to the reader: no role, no answer in the model's mouth.
            "notice" => row.text = str_of("text").unwrap_or_default(),
            // The reader sees which prompt was in force, not 10k characters of it.
            "system" => {
                let cwd = str_of("cwd").unwrap_or_default();
                let chars = str_of("instructions").unwrap_or_default().chars().count();
                let folded = value.get("handover").is_some_and(|v| !v.is_null());
                row.text = format!(
                    "[prompt in force] {chars} characters of instructions, \n\
                     {}a handover note, read from {cwd}",
                    if folded { "with " } else { "without " },
                );
            }
            // Something that ran at the provider, not here: it has no output to
            // show, so the line describes what it did instead.
            "builtin_call" => {
                let item = value.get("item").unwrap_or(&serde_json::Value::Null);
                row.text = activity_brief(item);
            }
            other => tracing::warn!("unknown message kind {other}"),
        }
        row
    }
}

/// The pictures a stored `tool_result` row recorded, by path.
pub(crate) fn images_of(value: &serde_json::Value) -> Vec<ImageRef> {
    value
        .get("images")
        .and_then(|v| v.as_array())
        .map(|list| list.iter().filter_map(ImageRef::from_json).collect())
        .unwrap_or_default()
}

/// The item a row is sent as: one standard Responses input item, decided once, here.
///
/// # Invariant
///
/// A request sends what is stored rather than re-deriving it, so the shape a provider
/// takes is computed on the way in. `None` is the row's own answer to "is any request
/// built from you": a notice, the prompt in force, a reasoning entry with no encrypted
/// blob, a provider item whose payload was stripped.
fn wire_item(kind: &str, value: &serde_json::Value) -> Option<serde_json::Value> {
    let text_of = |key: &str| {
        value
            .get(key)
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string()
    };
    match kind {
        "text" => {
            let role = text_of("role");
            let text = text_of("text");
            Some(if role == "assistant" {
                json!({"role": "assistant", "content": [{"type": "output_text", "text": text}]})
            } else {
                json!({"role": role, "content": text})
            })
        }
        "reasoning" => {
            let encrypted = value.get("encrypted").and_then(|v| v.as_str())?;
            let mut item = json!({
                "type": "reasoning",
                "summary": [],
                "encrypted_content": encrypted,
            });
            if let Some(id) = value.get("item_id").and_then(|v| v.as_str()) {
                item["id"] = json!(id);
            }
            Some(item)
        }
        "tool_call" => Some(json!({
            "type": "function_call",
            "call_id": text_of("call_id"),
            "name": text_of("name"),
            "arguments": text_of("arguments"),
        })),
        // The pictures ride in `display`: a stored item names a file, and the bytes
        // are read when the request is written.
        "tool_result" => Some(json!({
            "type": "function_call_output",
            "call_id": text_of("call_id"),
            "output": text_of("output"),
        })),
        "builtin_call" => {
            let item = value.get("item")?;
            if item.is_null() {
                return None;
            }
            let replayable = value
                .get("replayable")
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(true);
            replayable.then(|| item.clone())
        }
        _ => None,
    }
}

/// Moves every tool result up to the item directly after the call it answers.
///
/// # Invariant
///
/// A provider reads a `function_call` as answered by the input item that follows
/// it, while the rows land in completion order: a step that asked for two tools
/// stores whichever result came back first, so replaying the stored order leaves
/// the slower call reading as unanswered and the request is rejected.
fn pair_tool_results(items: Vec<HistoryItem>) -> Vec<HistoryItem> {
    let mut answers: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    for (index, item) in items.iter().enumerate() {
        if let HistoryItem::ToolResult { call_id, .. } = item {
            answers.insert(call_id.clone(), index);
        }
    }
    let mut slots: Vec<Option<HistoryItem>> = items.into_iter().map(Some).collect();
    let mut out = Vec::with_capacity(slots.len());
    for index in 0..slots.len() {
        let Some(item) = slots[index].take() else {
            continue;
        };
        let answer = match &item {
            HistoryItem::ToolCall { call_id, .. } => answers.get(call_id).copied(),
            _ => None,
        };
        out.push(item);
        if let Some(at) = answer {
            if let Some(result) = slots[at].take() {
                out.push(result);
            }
        }
    }
    out
}

/// The operator-facing name of a provider tool: `web_search_call` -> `web_search`.
pub(crate) fn activity_name(item_type: &str) -> &str {
    item_type.trim_end_matches("_call")
}

/// What an activity item says about itself: the operation, the query, the URL.
///
/// # Invariant
///
/// A leading space when there is something to say, nothing at all when there is
/// not: both readers append it, so an absent detail must not become `" "`.
/// Fields are looked up, never assumed: DeepSeek sends `action.queries` and
/// `action.type`, where other examples send `action.query`.
pub(crate) fn activity_detail(item: &serde_json::Value) -> String {
    let action = &item["action"];
    let name = activity_name(item["type"].as_str().unwrap_or("call"));
    let mut out = String::new();
    // DeepSeek reports an `open_page` under a `web_search_call`, so say the
    // operation when the name does not.
    if let Some(operation) = action["type"].as_str() {
        if !name.ends_with(operation) {
            out.push_str(&format!(" {operation}"));
        }
    }
    if let Some(query) = action["query"].as_str() {
        out.push_str(&format!(" {query:?}"));
    } else if let Some(queries) = action["queries"].as_array() {
        // A search may carry several terms; the rest stay out of a one-line row.
        if let Some(first) = queries.iter().filter_map(|q| q.as_str()).next() {
            out.push_str(&format!(" {first:?}"));
            let extra = queries.len().saturating_sub(1);
            if extra > 0 {
                out.push_str(&format!(" (+{extra} more)"));
            }
        }
    } else if let Some(url) = action["url"].as_str() {
        // A provider may tag the URL with its own call id; it is not part of it.
        let url = url.split("#ws_call_id=").next().unwrap_or(url);
        out.push_str(&format!(" {url}"));
    } else if let Some(text) = item["revised_prompt"].as_str() {
        out.push_str(&format!(" {text:?}"));
    }
    match item["status"].as_str() {
        None | Some("completed") | Some("in_progress") => {}
        Some(status) => out.push_str(&format!(" ({status})")),
    }
    out
}

/// One readable line for a provider tool item: `web_search "rust async book"`.
///
/// # Invariant
///
/// An absent field is left out, never rendered as `{}` and never guessed at:
/// this line is what `get_messages` shows, and a blank one reads as "nothing
/// happened here". See `docs/notes/server-store.md`.
pub(crate) fn activity_brief(item: &serde_json::Value) -> String {
    let Some(kind) = item["type"].as_str() else {
        return "a provider tool ran (its item was not recorded)".to_string();
    };
    format!("{}{}", activity_name(kind), activity_detail(item))
}

/// A transcript entry, already decoded into the shape the LLM adapter wants.
#[derive(Debug, Clone)]
pub enum HistoryItem {
    Text {
        role: String,
        text: String,
    },
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
        /// Pictures the tool handed back, by path: read at request time.
        images: Vec<ImageRef>,
    },
    /// A tool the provider ran itself (`web_search_call`, ...), kept verbatim
    /// so the next request can replay it.
    BuiltinCall {
        item: serde_json::Value,
        /// False when a binary payload was stripped and the item must not be sent back.
        replayable: bool,
    },
}

// One shape for every reader: the scan, the store and the request all agree.
pub use crate::image::ImageRef;

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

    /// Folds like the `heir` skill: a second connection, the same statement.
    /// The store owns no writer for a handover.
    fn fold_from_outside(dir: &std::path::Path, session_id: i64, note: &str) {
        let conn = rusqlite::Connection::open(dir.join("g.db")).unwrap();
        conn.execute(
            "UPDATE sessions SET handover = ?1 WHERE id = ?2",
            params![note, session_id],
        )
        .unwrap();
    }

    /// `--db <path>` brings its directory, and `:memory:` must not become a directory.
    #[test]
    fn the_database_path_brings_its_directory() {
        let dir = tempfile::tempdir().unwrap();
        let nested = dir.path().join("a").join("b").join("g.db");
        Store::open(&nested).unwrap();
        assert!(nested.is_file(), "the parent directories must be created");

        let store = Store::open(Path::new(":memory:")).unwrap();
        store.create_session("main", "m", "C:/x").unwrap();
    }

    /// Finishing one pending task must leave another queued message's row alone.
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
    fn a_new_turn_commits_its_obligation_and_first_message_together() {
        let (_d, store) = temp_store();
        let id = store.create_session("main", "m", "C:/x").unwrap();
        let task = store.begin_turn(id, "main", "hello").unwrap();
        assert!(store.pending_tasks().unwrap().iter().any(|t| t.id == task));
        let rows = store.messages_between(id, 0, i64::MAX).unwrap();
        assert_eq!(rows.messages.len(), 1);
        assert_eq!(rows.messages[0].kind, "text");

        assert!(store.begin_turn(i64::MAX, "broken", "rollback").is_err());
        assert!(
            !store
                .pending_tasks()
                .unwrap()
                .iter()
                .any(|t| t.session == "broken")
        );
    }

    #[test]
    fn inbox_delivery_is_atomic_and_does_not_consume_turn_rows() {
        let (_d, store) = temp_store();
        let id = store.create_session("main", "m", "C:/x").unwrap();
        let turn = store.begin_turn(id, "main", "first").unwrap();
        store
            .add_pending_task("main", "inject", &json!({"message": "second"}))
            .unwrap();
        store
            .add_pending_task("main", "inject", &json!({"message": "third"}))
            .unwrap();

        assert_eq!(store.deliver_pending(id, "main").unwrap(), 2);
        assert_eq!(store.deliver_pending(id, "main").unwrap(), 0);
        let pending = store.pending_tasks().unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].id, turn);
        assert_eq!(
            store
                .messages_between(id, 0, i64::MAX)
                .unwrap()
                .messages
                .len(),
            3
        );

        store
            .add_pending_task("main", "inject", &json!({"message": "kept"}))
            .unwrap();
        assert!(store.deliver_pending(i64::MAX, "main").is_err());
        assert!(
            store
                .pending_tasks()
                .unwrap()
                .iter()
                .any(|t| t.kind == "inject")
        );
    }

    /// A picture is a path in the row, not bytes: the request reads the file.
    #[test]
    fn a_tool_result_keeps_its_pictures_by_path() {
        let (_d, store) = temp_store();
        let id = store.create_session("main", "m", "C:/x").unwrap();
        store
            .append_message(
                id,
                "tool_call",
                &json!({"call_id": "c1", "name": "read_image", "arguments": "{}"}),
            )
            .unwrap();
        store
            .append_message(
                id,
                "tool_result",
                &json!({
                    "call_id": "c1",
                    "output": "looked at it",
                    "images": [{"path": "C:/w/shot.png", "media_type": "image/png"}]
                }),
            )
            .unwrap();
        let history = store.history(id).unwrap();
        match &history[1] {
            HistoryItem::ToolResult { output, images, .. } => {
                assert_eq!(output, "looked at it");
                assert_eq!(images, &vec![ImageRef::new("C:/w/shot.png", "image/png")]);
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    /// A row written before pictures existed still reads back as a plain result.
    #[test]
    fn a_tool_result_without_pictures_reads_back_empty() {
        let (_d, store) = temp_store();
        let id = store.create_session("main", "m", "C:/x").unwrap();
        store
            .append_message(
                id,
                "tool_call",
                &json!({"call_id": "c1", "name": "bash", "arguments": "{}"}),
            )
            .unwrap();
        store
            .append_message(id, "tool_result", &json!({"call_id": "c1", "output": "ok"}))
            .unwrap();
        match &store.history(id).unwrap()[1] {
            HistoryItem::ToolResult { images, .. } => assert!(images.is_empty()),
            other => panic!("unexpected {other:?}"),
        }
    }

    /// The provider's own tool item is kept verbatim, or the next request is rejected.
    #[test]
    fn a_provider_tool_item_survives_a_round_trip() {
        let (_d, store) = temp_store();
        let id = store.create_session("main", "m", "C:/x").unwrap();
        let item = json!({
            "id": "ws_1",
            "type": "web_search_call",
            "status": "completed",
            "action": {"type": "search", "query": "q"}
        });
        store
            .append_message(
                id,
                "builtin_call",
                &json!({"item": item, "replayable": true}),
            )
            .unwrap();
        match &store.history(id).unwrap()[0] {
            HistoryItem::BuiltinCall {
                item: kept,
                replayable,
            } => {
                assert_eq!(kept, &item);
                assert!(replayable);
            }
            other => panic!("unexpected {other:?}"),
        }
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

    /// A result whose call is gone is not sent: a lone output is rejected.
    #[test]
    fn a_result_without_its_call_is_not_sent() {
        let (_d, store) = temp_store();
        let id = store.create_session("s", "m", "C:/x").unwrap();
        store
            .append_message(id, "text", &json!({"role":"user","text":"ask"}))
            .unwrap();
        store
            .append_message(
                id,
                "tool_result",
                &json!({"call_id":"call_gone","output":"rows moved"}),
            )
            .unwrap();
        let (_, items) = store.context(id).unwrap();
        assert_eq!(items.len(), 1, "the orphan is dropped: {items:?}");

        store
            .append_message(
                id,
                "tool_call",
                &json!({"call_id":"call_here","name":"bash","arguments":"{}"}),
            )
            .unwrap();
        store
            .append_message(
                id,
                "tool_result",
                &json!({"call_id":"call_here","output":"ok"}),
            )
            .unwrap();
        let (_, items) = store.context(id).unwrap();
        assert_eq!(items.len(), 3, "text, call, result: {items:?}");
    }

    /// A handover note shrinks the request but never touches the transcript rows.
    #[test]
    fn a_folded_prefix_shrinks_the_request_without_touching_the_transcript() {
        let (d, store) = temp_store();
        let id = store.create_session("s", "m", "C:/x").unwrap();
        for i in 0..5 {
            store
                .append_message(
                    id,
                    "text",
                    &serde_json::json!({"role":"user","text":format!("m{i}")}),
                )
                .unwrap();
        }
        assert_eq!(store.handover(id).unwrap(), None);

        fold_from_outside(d.path(), id, "the first three");

        // A handover leaves the note behind and nothing else: the rows it
        // covered are in the mentor, so every row still here is sent.
        let (note, items) = store.context(id).unwrap();
        assert_eq!(note.as_deref(), Some("the first three"));
        assert_eq!(items.len(), 5, "everything the session still holds");
        assert_eq!(store.history(id).unwrap().len(), 5);
        assert_eq!(store.message_index(id).unwrap().len(), 5);
    }

    /// `set_session` tuning is per-session state, so it must survive a restart.
    #[test]
    fn session_tuning_round_trips() {
        let (_d, store) = temp_store();
        let id = store
            .create_session("s", "MiniMax-M3.1-Flash", "/tmp")
            .unwrap();
        let row = store.get_session("s").unwrap().unwrap();
        assert_eq!(row.llm, None, "a session starts on the server's own entry");
        assert_eq!(row.context_limit, None);
        assert_eq!(row.thinking_effort, None, "no level means the model's own");

        store
            .set_session_tuning(
                id,
                Some("deepseek-official/deepseek-flash"),
                Some(30),
                Some("high"),
                "deepseek-flash",
            )
            .unwrap();
        let row = store.get_session("s").unwrap().unwrap();
        assert_eq!(row.llm.as_deref(), Some("deepseek-official/deepseek-flash"));
        assert_eq!(row.context_limit, Some(30));
        assert_eq!(row.thinking_effort.as_deref(), Some("high"));
        assert_eq!(row.model, "deepseek-flash");

        // Splitting by sequence adds up; counting rows does not, so the halves must match.
        store
            .append_message(id, "text", &serde_json::json!({"role":"user","text":"a"}))
            .unwrap();
        store
            .append_message(id, "notice", &serde_json::json!({"text":"note"}))
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
                "text",
                &serde_json::json!({"role":"assistant","text":"b"}),
            )
            .unwrap();
        let all = store.history_between(id, 0, i64::MAX).unwrap();
        // Four rows, four items: the notice adds none, the unanswered call adds two.
        assert_eq!(all.len(), 4);
        for (seq, _) in store.message_index(id).unwrap() {
            let old = store.history_between(id, 0, seq).unwrap();
            let new = store.history_between(id, seq, i64::MAX).unwrap();
            assert_eq!(
                old.len() + new.len(),
                all.len(),
                "splitting at seq {seq} lost or duplicated an item"
            );
        }

        // Clearing goes back to the server's model, which only the caller knows.
        store
            .set_session_tuning(id, None, None, None, "MiniMax-M3.1-Flash")
            .unwrap();
        let row = store.get_session("s").unwrap().unwrap();
        assert_eq!(row.llm, None);
        assert_eq!(row.context_limit, None);
        assert_eq!(row.model, "MiniMax-M3.1-Flash");
    }

    /// A database written before the note had a column must grow, not fail.
    #[test]
    fn opening_an_old_database_adds_the_new_columns() {
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
        // No such session yet: the read fails, and "no note" is the honest answer.
        assert_eq!(store.handover(1).unwrap_or(None), None);
        // The tuning columns arrive the same way; a session that predates them
        // reads as "no overrides", not as an error.
        assert!(store.list_sessions().is_ok());
    }

    #[test]
    fn detached_jobs_survive_until_stopped() {
        let (_d, store) = temp_store();
        let id = store
            .add_detached_job(&DetachedRow {
                id: 0,
                session: "main".into(),
                name: "svc".into(),
                shell: "svc".into(),
                job: 1,
                pid: 4242,
                cmd: "gongga-server --bind 127.0.0.1:9800".into(),
                log: "C:/tmp/main-1.log".into(),
                started_at: 7,
            })
            .unwrap();
        let rows = store.detached_jobs().unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].name, "svc");
        assert_eq!(rows[0].job, 1);
        assert_eq!(rows[0].pid, 4242);
        assert_eq!(rows[0].log, "C:/tmp/main-1.log");
        store.delete_detached_job(id).unwrap();
        assert!(store.detached_jobs().unwrap().is_empty());
    }

    /// The measured context size survives a restart, for the next run's first request.
    #[test]
    fn the_measured_context_size_survives_a_restart() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("g.db");
        let id = {
            let store = Store::open(&path).unwrap();
            let id = store.create_session("main", "m", "C:/x").unwrap();
            // Never measured: zero, which the agent reads as "small enough".
            assert_eq!(store.context_tokens(id).unwrap(), 0);
            store.set_context_tokens(id, 12_345).unwrap();
            id
        };
        let reopened = Store::open(&path).unwrap();
        assert_eq!(reopened.context_tokens(id).unwrap(), 12_345);
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

    /// Events have no foreign key, so deleting a session must take them explicitly.
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
            .query_row(
                "SELECT COUNT(*) FROM events WHERE session_id = ?1",
                params![id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(n, 0);
    }

    /// An interrupted call is closed for the model, or every later turn fails too.
    #[test]
    fn a_call_interrupted_before_its_result_is_closed_for_the_model() {
        let (_d, store) = temp_store();
        let id = store.create_session("main", "m", "C:/x").unwrap();
        store
            .append_message(
                id,
                "tool_call",
                &serde_json::json!({"call_id": "c1", "name": "bash", "arguments": "{}"}),
            )
            .unwrap();
        // A second call that did come back must not be closed twice.
        store
            .append_message(
                id,
                "tool_call",
                &serde_json::json!({"call_id": "c2", "name": "bash", "arguments": "{}"}),
            )
            .unwrap();
        store
            .append_message(
                id,
                "tool_result",
                &serde_json::json!({"call_id": "c2", "output": "ok"}),
            )
            .unwrap();

        let history = store.history(id).unwrap();
        let pairs: Vec<(String, String)> = history
            .iter()
            .map(|item| match item {
                HistoryItem::ToolCall { call_id, .. } => (call_id.clone(), "call".into()),
                HistoryItem::ToolResult {
                    call_id, output, ..
                } => (call_id.clone(), format!("result: {output}")),
                _ => ("?".into(), "other".into()),
            })
            .collect();
        assert_eq!(pairs.len(), 4, "{pairs:?}");
        // c1 gets a closing row too, in order; its words are the template's.
        assert_eq!(pairs[0], ("c1".to_string(), "call".to_string()));
        assert_eq!(pairs[1].0, "c1", "{pairs:?}");
        assert_eq!(pairs[2], ("c2".to_string(), "call".to_string()));
        assert_eq!(pairs[3].1, "result: ok");
    }

    /// What ran at the provider has no output to show, so the row describes it.
    #[test]
    fn a_provider_tool_row_reads_as_the_search_it_ran() {
        let (_d, store) = temp_store();
        let id = store.create_session("main", "m", "C:/x").unwrap();
        store
            .append_message(
                id,
                "builtin_call",
                &json!({
                    "item": {
                        "id": "ws_1",
                        "type": "web_search_call",
                        "status": "completed",
                        "action": {"type": "search", "query": "rust async book"}
                    },
                    "replayable": true
                }),
            )
            .unwrap();
        store
            .append_message(
                id,
                "builtin_call",
                &json!({
                    "item": {
                        "id": "wf_1",
                        "type": "web_fetch_call",
                        "status": "completed",
                        "action": {"type": "open_page", "url": "https://example.org/a"}
                    },
                    "replayable": true
                }),
            )
            .unwrap();

        let t = store.messages_between(id, 1, i64::MAX).unwrap();
        assert_eq!(t.messages[0].kind, "builtin_call");
        assert_eq!(t.messages[0].text, "web_search \"rust async book\"");
        // The operation is shown when the item's own name does not say it.
        assert_eq!(
            t.messages[1].text,
            "web_fetch open_page https://example.org/a"
        );
    }

    /// The shape DeepSeek actually sent: `queries`, `open_page`, call-id fragment.
    ///
    /// # Invariant
    ///
    /// Copied from a real `deepseek-v4-pro` response, so a provider whose
    /// action is spelled differently keeps its terms.
    #[test]
    fn a_deepseek_web_search_row_keeps_its_terms() {
        let (_d, store) = temp_store();
        let id = store.create_session("main", "m", "C:/x").unwrap();
        for item in [
            json!({
                "id": "call_01",
                "type": "web_search_call",
                "status": "completed",
                "action": {"type": "search", "queries": [
                    "site:news.ycombinator.com top story today",
                    "Hacker News front page current top story",
                    "ws_call_id=call_01"
                ]}
            }),
            json!({
                "id": "call_00",
                "type": "web_search_call",
                "status": "failed",
                "action": {
                    "type": "open_page",
                    "url": "https://news.ycombinator.com/#ws_call_id=call_00"
                }
            }),
        ] {
            store
                .append_message(
                    id,
                    "builtin_call",
                    &json!({"item": item, "replayable": true}),
                )
                .unwrap();
        }
        let t = store.messages_between(id, 1, i64::MAX).unwrap();
        assert_eq!(
            t.messages[0].text,
            "web_search \"site:news.ycombinator.com top story today\" (+2 more)"
        );
        // The fragment is the provider's call id, not part of the page.
        assert_eq!(
            t.messages[1].text,
            "web_search open_page https://news.ycombinator.com/ (failed)"
        );
    }

    /// An item that says nothing still gets a line: a blank row reads as nothing.
    #[test]
    fn a_provider_tool_row_never_renders_blank() {
        let (_d, store) = temp_store();
        let id = store.create_session("main", "m", "C:/x").unwrap();
        for body in [
            json!({"item": {"id": "x", "type": "web_search_call"}, "replayable": true}),
            json!({"item": {"id": "y", "type": "image_generation_call", "status": "failed"}, "replayable": true}),
            json!({"item": {"id": "z"}, "replayable": true}),
            json!({"replayable": true}),
        ] {
            store.append_message(id, "builtin_call", &body).unwrap();
        }
        let t = store.messages_between(id, 1, i64::MAX).unwrap();
        for row in &t.messages {
            assert!(!row.text.trim().is_empty(), "blank row for {:?}", row.seq);
            assert!(
                !row.text.contains("{}"),
                "shape leaked into the line: {}",
                row.text
            );
        }
        // A bare tool still names itself, with no trailing space to trim.
        assert_eq!(t.messages[0].text, "web_search");
        assert_eq!(t.messages[1].text, "image_generation (failed)");
        // An item with no type at all says that much rather than guessing.
        assert!(
            t.messages[2].text.contains("a provider tool ran"),
            "{}",
            t.messages[2].text
        );
        assert_eq!(
            t.messages[3].text,
            "a provider tool ran (its item was not recorded)"
        );
    }

    /// The prompt row is what a request is built from, not a row inside it.
    #[test]
    fn a_prompt_row_is_sent_and_never_repeated_as_history() {
        let (d, store) = temp_store();
        let id = store.create_session("main", "m", "C:/x").unwrap();
        fold_from_outside(d.path(), id, "the note");
        let prompt = Prompt {
            instructions: "You are gongga".into(),
            handover: Some("Earlier in this session: the note".into()),
            cwd: "C:/x".into(),
            stamp: stamp(Some("the note")),
        };
        store.append_prompt(id, &prompt).unwrap();
        store
            .append_message(id, "text", &json!({"role": "user", "text": "go"}))
            .unwrap();

        let (_, history) = store.context(id).unwrap();
        assert_eq!(
            history.len(),
            1,
            "the prompt row is not history: {history:?}"
        );

        let read = store
            .prompt(id)
            .unwrap()
            .expect("the newest row reads back");
        assert_eq!(read.instructions, "You are gongga");
        assert_eq!(read.cwd, "C:/x");
        assert_eq!(read.stamp, stamp(Some("the note")));

        // A reader of the transcript gets the shape of it, not 10k characters.
        let t = store.messages_between(id, 1, i64::MAX).unwrap();
        assert!(
            t.messages[0].text.contains("[prompt in force]"),
            "{:?}",
            t.messages[0].text
        );
        assert_eq!(t.messages[1].kind, "text");
    }

    /// A call left open by the last turn gets its result written, once.
    #[test]
    fn a_dangling_call_is_closed_with_a_row_not_a_fiction() {
        let (_d, store) = temp_store();
        let id = store.create_session("main", "m", "C:/x").unwrap();
        store
            .append_message(
                id,
                "tool_call",
                &json!({"call_id": "c1", "name": "bash", "arguments": "{}"}),
            )
            .unwrap();

        assert_eq!(store.close_dangling_calls(id).unwrap(), 1);
        assert_eq!(
            store.close_dangling_calls(id).unwrap(),
            0,
            "closed once is closed"
        );
        let rows = store.messages_between(id, 1, i64::MAX).unwrap();
        assert_eq!(
            rows.messages.len(),
            2,
            "the closure is a row: {:?}",
            rows.messages
        );
        assert_eq!(rows.messages[1].kind, "tool_result");
    }

    /// A payload another session still names is not this session's to take with it.
    #[test]
    fn only_an_unreferenced_blob_is_orphaned() {
        let (_d, store) = temp_store();
        let a = store.create_session("a", "m", "C:/x").unwrap();
        let b = store.create_session("b", "m", "C:/x").unwrap();
        let item = |id: &str| json!({"id": id, "type": "image_generation_call", "result_blob": id});
        for (sid, id) in [(a, "ig_1"), (a, "ig_1"), (a, "ig_2"), (b, "ig_2")] {
            store
                .append_message(
                    sid,
                    "builtin_call",
                    &json!({"item": item(id), "replayable": true}),
                )
                .unwrap();
        }

        let mut orphans = store.orphaned_blobs("a").unwrap();
        orphans.sort();
        // `ig_1` twice and nowhere else; `ig_2` is b's too.
        assert_eq!(orphans, vec!["ig_1".to_string()]);
        assert!(
            store.orphaned_blobs("b").unwrap().is_empty(),
            "b gives nothing up while a names ig_2"
        );

        store.delete_session("a").unwrap();
        let history = store.history_since(b, 0).unwrap();
        assert_eq!(history.len(), 1, "b's row is intact: {history:?}");
        assert_eq!(store.orphaned_blobs("b").unwrap(), vec!["ig_2".to_string()]);
    }

    /// A notice is readable but never sent back to the model, or it answers the harness.
    #[test]
    fn a_notice_is_readable_but_never_sent_back_to_the_model() {
        let (_d, store) = temp_store();
        let id = store.create_session("main", "m", "C:/x").unwrap();
        store
            .append_message(
                id,
                "text",
                &serde_json::json!({"role": "user", "text": "do the thing"}),
            )
            .unwrap();
        store
            .append_message(
                id,
                "notice",
                &serde_json::json!({"text": "stopped after 64 steps"}),
            )
            .unwrap();

        let (_, history) = store.context(id).unwrap();
        assert_eq!(history.len(), 1, "the notice is not history: {history:?}");

        let t = store.messages_between(id, 1, i64::MAX).unwrap();
        assert_eq!(t.last_seq, 2);
        assert_eq!(t.messages[1].kind, "notice");
        assert_eq!(t.messages[1].text, "stopped after 64 steps");
        assert_eq!(t.messages[1].role, None);
    }

    /// The window carries the status of *that* session.
    ///
    /// # Invariant
    ///
    /// Rows and status come from one statement, so a reader is not told about a
    /// session that moved on in between.
    ///
    /// Both readings come from one statement, so the pair cannot disagree.
    #[test]
    fn a_transcript_window_reports_the_session_status() {
        let (_dir, store) = temp_store();
        let a = store.create_session("a", "m", "C:/x").unwrap();
        let b = store.create_session("b", "m", "C:/x").unwrap();
        store
            .append_message(a, "text", &json!({"role": "user", "text": "hi"}))
            .unwrap();

        // A fresh session is idle; a running one says running.
        assert_eq!(
            store.messages_between(a, 1, i64::MAX).unwrap().status,
            SessionStatus::Idle
        );
        store.set_status(a, SessionStatus::Running).unwrap();
        assert_eq!(
            store.messages_between(a, 1, i64::MAX).unwrap().status,
            SessionStatus::Running
        );
        // The other session is untouched: the status is per session, not global.
        assert_eq!(
            store.messages_between(b, 1, i64::MAX).unwrap().status,
            SessionStatus::Idle
        );
        // An error stays an error: a poller must not read it as finished.
        store.set_status(a, SessionStatus::Error).unwrap();
        assert_eq!(
            store.messages_between(a, 1, i64::MAX).unwrap().status,
            SessionStatus::Error
        );

        // Reading a window never creates a session and never moves the status.
        let before = store.list_sessions().unwrap().len();
        store.messages_between(a, 5, 9).unwrap();
        assert_eq!(store.list_sessions().unwrap().len(), before);
        assert_eq!(
            store.messages_between(a, 5, 9).unwrap().status,
            SessionStatus::Error
        );
    }

    /// An id that was never created reads as an empty window.
    ///
    /// # Invariant
    ///
    /// `messages_between` is given an id the caller already resolved: the "no such
    /// session" refusal belongs to the lookup, and is pinned next door.
    #[test]
    fn an_unknown_session_id_has_no_rows_and_no_status() {
        let (_dir, store) = temp_store();
        let t = store.messages_between(424242, 1, i64::MAX).unwrap();
        assert_eq!(t.last_seq, 0);
        assert!(t.messages.is_empty());
        assert_eq!(
            t.status,
            SessionStatus::Idle,
            "parse_status defaults, it does not invent a row"
        );
        assert!(
            store.list_sessions().unwrap().is_empty(),
            "and nothing was created"
        );
    }
}
