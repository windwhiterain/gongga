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
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Prompt {
    /// The system instructions this session was sent under, verbatim.
    #[serde(default)]
    pub instructions: String,
    /// The text of the references the first message named, in the order it is sent.
    #[serde(default)]
    pub index: Option<String>,
    /// The start card: name, build, the first turn's time and cwd. Stamped once.
    #[serde(default)]
    pub tail: Option<String>,
    /// The spellings `index` carries, so a mention of one stays literal.
    #[serde(default)]
    pub rooted: Vec<String>,
    /// The directory the shell stood in, so a move is noticed.
    #[serde(default)]
    pub cwd: String,
    /// The peer that directory was on, when it was one.
    ///
    /// # Invariant
    ///
    /// A record, like `cwd`, and read for the same thing: `--remote none` in the
    /// directory the session left is still a move, and the card that says "on remote
    /// `x`" must not survive it. A row written before this field existed has `None`,
    /// which is a session on a peer getting its card rewritten once.
    #[serde(default)]
    pub remote: Option<String>,
}

/// The row that closes a turn: see `docs/notes/server-store.md`.
///
/// # Invariant
///
/// Fixed text, because every later request of the session carries it.
pub const THINKING_MARK: &str = "(thinking enabled)";

/// What the model is told about a call that never returned: the
/// `interrupted_call` template.
pub fn interrupted_call() -> String {
    crate::prompts::shared()
        .text("interrupted_call")
        .unwrap_or_default()
}

const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS sessions (
    id         INTEGER PRIMARY KEY,
    name       TEXT NOT NULL UNIQUE,
    status     TEXT NOT NULL DEFAULT 'idle',
    -- The model this session runs on, `provider/model_name`. Not the model's own
    -- name: a name alone does not say who serves it.
    model      TEXT NOT NULL,
    -- The name a row was created with, before `model` carried the provider too.
    -- Only a row that predates that spelling reads it; nothing writes it now.
    model_name TEXT,
    cwd        TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL,
    -- Per-session overrides given by `set_session`. NULL means "the server's
    -- configuration", so a session that never asked is indistinguishable from
    -- one created before these columns existed. `model` holds the whole model,
    -- `provider/model_name`; `model_name` above is the row's own name for it.
    context_limit INTEGER,
    -- How hard the model should think, in the roster's spelling. NULL means the
    -- model's own default, which is what a session that never asked should get.
    thinking_effort TEXT,
    -- Input tokens the provider reported for the last request this session
    -- made. Not an estimate: it is the number the provider billed, which is the
    -- only honest answer to "how full is the context". Surviving a restart is
    -- the point -- the first request of the next run needs a size to compare
    -- against, and no amount of character counting would give a better one.
    context_tokens INTEGER NOT NULL DEFAULT 0,
    -- Incremented by the reset trigger, so a notice queued for an older transcript
    -- stays obsolete even when the in-flight request writes its old usage back.
    context_generation INTEGER NOT NULL DEFAULT 0,
    -- What of that the provider served from its prefix cache: SPEC's
    -- `cached (in n%, latest)` is a share of the same number. NULL is a provider
    -- that does not report it, which is not the same as nothing cached.
    cached_tokens INTEGER,
    -- Output tokens per second, and milliseconds to the first token, of the last
    -- completed request: SPEC's `tps (latest)` and `ttft (latest)`, kept beside the
    -- context so a window attached now has the numbers without replaying a turn.
    tps REAL,
    ttft_ms INTEGER,
    -- The endpoint this session's shells run on, as a named remote from the
    -- server's config. NULL is this machine, and it is fixed once the session has run.
    remote TEXT,
    -- The last row of this session a monitor turn was given. A turn writes its
    -- user row before it starts, so `seq` only grows: this column is the whole
    -- record of "this failure has been reported", and it survives a restart.
    monitor_seq INTEGER NOT NULL DEFAULT 0,
    -- How many models a monitor turn has been switched through for the failure
    -- this session is in. Reset when the session stops being in error, so a new
    -- failure starts the search again.
    monitor_attempts INTEGER NOT NULL DEFAULT 0,
    -- The tier every request of this session sends, in the roster's spelling.
    -- NULL picks the shortest tier the roster declares, so a row written before
    -- this column existed reads what a session that never asked reads.
    cache_tier TEXT,
    -- Whether a periodic time notice keeps this session's prefix warm. Off by
    -- default: a session that never asked starts no timer of its own.
    cache_keepalive INTEGER NOT NULL DEFAULT 0,
    -- When this session's last turn ended, and nothing else. NULL is a session
    -- that has not finished a turn yet, which the list orders by `created_at`.
    -- Its own column rather than `updated_at`, which a tuning change rewrites:
    -- the list is ordered by this, so writing it anywhere else would move a row
    -- for a reason the operator did not give.
    ended_at INTEGER
);
CREATE INDEX IF NOT EXISTS sessions_by_status ON sessions(status);

-- A session's rows can still be deleted -- by the agent, from a shell, through the
-- `heir` skill, and only after the whole transcript has been copied into a mentor
-- session and the copy checked row for row. The server's own hands stay off the
-- transcript.
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

-- What a kind of monitor has already been handed. A table of its own because the
-- two kinds count different things -- a transcript's row for one, the event log's
-- id for the other -- and neither belongs in a column named after a session.
-- See `docs/notes/server-monitor.md`.
CREATE TABLE IF NOT EXISTS monitor_state (
    kind  TEXT PRIMARY KEY,
    value INTEGER NOT NULL
);

-- What a stream has said so far, while it is saying it. Not the transcript: a row
-- here is rewritten as chunks arrive and deleted when the finished item is appended,
-- which is how half a message survives a process that is killed mid-stream. At the
-- next start every draft becomes a row marked `interrupted`, so the reader keeps what
-- it saw and the transcript keeps what it was sent. See `docs/notes/server-store.md`.
CREATE TABLE IF NOT EXISTS drafts (
    session_id INTEGER NOT NULL,
    item_id    TEXT NOT NULL,
    turn       INTEGER NOT NULL,
    part       TEXT NOT NULL,
    text       TEXT NOT NULL,
    updated_at INTEGER NOT NULL,
    PRIMARY KEY (session_id, item_id)
);

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
    started_at INTEGER NOT NULL,
    -- Whether the session has been told this job ended. Exactly one notice per job,
    -- across restarts: the watcher sets it, and adoption reads it.
    noticed    INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX IF NOT EXISTS detached_by_session ON detached_jobs(session);

-- A tool call that did not wait for its answer: `background` keeps that answer for
-- `check_job`, `detached` hands it to the session when it ends. A job of this server
-- has no pid to adopt, so the row is the only trace -- and the only way a session
-- learns what a stopped run still owed it. See `docs/notes/server-tools-tunnel.md`.
CREATE TABLE IF NOT EXISTS tool_jobs (
    id          INTEGER PRIMARY KEY,
    session     TEXT NOT NULL,
    call_id     TEXT NOT NULL,
    tool        TEXT NOT NULL,
    description TEXT,
    -- `background` or `detached`: what the caller asked for, which decides who
    -- reads the answer.
    mode        TEXT NOT NULL,
    -- running, ok, error or lost.
    status      TEXT NOT NULL,
    output      TEXT,
    -- Whether the session has been handed the output: read by `check_job`, or
    -- carried by a `detached` notice.
    collected   INTEGER NOT NULL DEFAULT 0,
    started_at  INTEGER NOT NULL,
    finished_at INTEGER,
    -- A `detached` call runs in a process of its own: the pid is how the next start
    -- finds it again, and `answer` is the file that process writes.
    pid         INTEGER,
    answer      TEXT
);
CREATE INDEX IF NOT EXISTS tool_jobs_by_session ON tool_jobs(session);

-- What the prefix cache has learnt about the order things are emitted in, one row per
-- owner -- the unit whose requests share bytes, which is a session's directory. Not one
-- per caller and not one per session: two sessions of one directory must plan from the
-- same orderings or their heads differ. See `docs/notes/server-prefix-cache.md`.
-- What a request of a model was measured at: characters written against the tokens
-- the provider counted. The ratio is the only honest way to cost an item.
CREATE TABLE IF NOT EXISTS token_ratio (
    model  TEXT PRIMARY KEY,
    chars  INTEGER NOT NULL,
    tokens INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS prefix_layout (
    owner  TEXT PRIMARY KEY,
    layout TEXT NOT NULL
);

-- What a session asked to be told about: a named pattern over the event
-- stream, who hears about a match, and what runs when it fires. `persistent`
-- decides whether a subscription survives its own match. See
-- `docs/notes/server-subscriptions.md`.
CREATE TABLE IF NOT EXISTS subscriptions (
    id         INTEGER PRIMARY KEY,
    session    TEXT NOT NULL,
    name       TEXT NOT NULL DEFAULT '',
    pattern    TEXT NOT NULL,
    persistent INTEGER NOT NULL,
    bash       TEXT,
    cmd        TEXT,
    python     TEXT,
    code       TEXT,
    created_at INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS subscriptions_by_session ON subscriptions(session);

-- The pictures a message named, copied into this home: the row names the copy by
-- id, and the bytes live beside the database. See `docs/notes/server-pictures.md`.
CREATE TABLE IF NOT EXISTS pictures (
    id         INTEGER PRIMARY KEY,
    path       TEXT NOT NULL,
    media_type TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS pictures_path ON pictures(path);

-- The provider file a copy was uploaded as, and when that file expires. The row a
-- request is built from never holds a provider id: it holds the copy, and the id is
-- looked up here, because a provider deletes its files and a transcript may not.
-- See `docs/notes/server-pictures.md`.
CREATE TABLE IF NOT EXISTS uploads (
    scope      TEXT NOT NULL,
    copy       INTEGER NOT NULL REFERENCES pictures(id) ON DELETE CASCADE,
    file_id    TEXT NOT NULL,
    expires_at INTEGER NOT NULL,
    PRIMARY KEY (scope, copy)
);
CREATE INDEX IF NOT EXISTS uploads_by_id ON uploads(file_id);
"#;

/// What a character costs when nothing has been measured; see `docs/notes/server-store.md`.
pub const CHARS_PER_TOKEN: f64 = 4.0;

#[derive(Debug, Clone)]
pub struct Store {
    conn: Arc<Mutex<Connection>>,
    /// Where the database is, so a request can open its own read-only view of it.
    path: PathBuf,
    live_compact_enabled: Arc<std::sync::atomic::AtomicBool>,
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

/// Gives a row written before `ended_at` existed the turn end it already had.
///
/// # Invariant
///
/// Only a session that has rows and no stamp: the last row of a transcript is
/// written by the last turn, so its time is when that turn ended, while a
/// session with no rows never ended one and keeps `NULL`. Safe at every start,
/// which is what makes a home an older build wrote right again.
/// See `docs/notes/server-store.md`.
fn backfill_ended_at(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute(
        "UPDATE sessions
            SET ended_at = (SELECT MAX(m.created_at) FROM messages m
                             WHERE m.session_id = sessions.id)
          WHERE ended_at IS NULL
            AND EXISTS (SELECT 1 FROM messages m WHERE m.session_id = sessions.id)",
        [],
    )?;
    Ok(())
}

/// Whether `table` already answers a select on `column`.
fn has_column(conn: &Connection, table: &str, column: &str) -> bool {
    conn.prepare(&format!("SELECT {column} FROM {table} LIMIT 1"))
        .is_ok()
}

/// Adds columns a `CREATE TABLE IF NOT EXISTS` cannot add to an old database.
fn migrate(conn: &Connection) -> rusqlite::Result<()> {
    // Renamed twice as the words settled: `llm` -> the model, `model` -> `model_name`.
    if has_column(conn, "sessions", "llm") && !has_column(conn, "sessions", "provider") {
        conn.execute("ALTER TABLE sessions RENAME COLUMN llm TO provider", [])?;
    }
    // A layout belongs to the bytes, not a session: its owner is a directory now.
    if has_column(conn, "prefix_layout", "session") && !has_column(conn, "prefix_layout", "owner") {
        conn.execute(
            "ALTER TABLE prefix_layout RENAME COLUMN session TO owner",
            [],
        )?;
    }
    if has_column(conn, "sessions", "model") && !has_column(conn, "sessions", "model_name") {
        conn.execute("ALTER TABLE sessions RENAME COLUMN model TO model_name", [])?;
    }
    if has_column(conn, "sessions", "provider") && !has_column(conn, "sessions", "model") {
        conn.execute("ALTER TABLE sessions RENAME COLUMN provider TO model", [])?;
    }
    for (column, ddl) in [
        ("model", "ALTER TABLE sessions ADD COLUMN model TEXT"),
        (
            "model_name",
            "ALTER TABLE sessions ADD COLUMN model_name TEXT",
        ),
        (
            "context_limit",
            "ALTER TABLE sessions ADD COLUMN context_limit INTEGER",
        ),
        (
            "context_tokens",
            "ALTER TABLE sessions ADD COLUMN context_tokens INTEGER NOT NULL DEFAULT 0",
        ),
        (
            "context_generation",
            "ALTER TABLE sessions ADD COLUMN context_generation INTEGER NOT NULL DEFAULT 0",
        ),
        (
            "thinking_effort",
            "ALTER TABLE sessions ADD COLUMN thinking_effort TEXT",
        ),
        (
            "cached_tokens",
            "ALTER TABLE sessions ADD COLUMN cached_tokens INTEGER",
        ),
        ("tps", "ALTER TABLE sessions ADD COLUMN tps REAL"),
        ("remote", "ALTER TABLE sessions ADD COLUMN remote TEXT"),
        ("ttft_ms", "ALTER TABLE sessions ADD COLUMN ttft_ms INTEGER"),
        (
            "monitor_seq",
            "ALTER TABLE sessions ADD COLUMN monitor_seq INTEGER NOT NULL DEFAULT 0",
        ),
        (
            "monitor_attempts",
            "ALTER TABLE sessions ADD COLUMN monitor_attempts INTEGER NOT NULL DEFAULT 0",
        ),
        (
            "cache_tier",
            "ALTER TABLE sessions ADD COLUMN cache_tier TEXT",
        ),
        (
            "cache_keepalive",
            "ALTER TABLE sessions ADD COLUMN cache_keepalive INTEGER NOT NULL DEFAULT 0",
        ),
    ] {
        add_column_if_missing(conn, "sessions", column, ddl)?;
    }
    add_column_if_missing(
        conn,
        "sessions",
        "ended_at",
        "ALTER TABLE sessions ADD COLUMN ended_at INTEGER",
    )?;
    backfill_ended_at(conn)?;
    settle_handover(conn)?;
    // An old row needs a name before it can be listed or deleted.
    add_column_if_missing(
        conn,
        "detached_jobs",
        "name",
        "ALTER TABLE detached_jobs ADD COLUMN name TEXT NOT NULL DEFAULT ''",
    )?;
    add_column_if_missing(
        conn,
        "detached_jobs",
        "noticed",
        "ALTER TABLE detached_jobs ADD COLUMN noticed INTEGER NOT NULL DEFAULT 0",
    )?;
    conn.execute(
        "UPDATE detached_jobs SET name = shell || '-' || job WHERE name = ''",
        [],
    )?;
    // A `detached` call became one kind of job. Work a stop left owed still is.
    if has_column(conn, "detached_calls", "call_id") {
        conn.execute_batch(
            "INSERT INTO tool_jobs (session, call_id, tool, description, mode, status, started_at)
             SELECT session, call_id, tool, description, 'detached', 'running', started_at
               FROM detached_calls
              WHERE NOT EXISTS (SELECT 1 FROM tool_jobs j WHERE j.call_id = detached_calls.call_id);
             DROP TABLE detached_calls;",
        )?;
    }
    add_column_if_missing(
        conn,
        "tool_jobs",
        "pid",
        "ALTER TABLE tool_jobs ADD COLUMN pid INTEGER",
    )?;
    add_column_if_missing(
        conn,
        "tool_jobs",
        "answer",
        "ALTER TABLE tool_jobs ADD COLUMN answer TEXT",
    )?;
    add_column_if_missing(
        conn,
        "messages",
        "call_id",
        "ALTER TABLE messages ADD COLUMN call_id TEXT",
    )?;
    add_column_if_missing(
        conn,
        "subscriptions",
        "name",
        "ALTER TABLE subscriptions ADD COLUMN name TEXT NOT NULL DEFAULT ''",
    )?;
    for (column, ddl) in [
        ("bash", "ALTER TABLE subscriptions ADD COLUMN bash TEXT"),
        ("cmd", "ALTER TABLE subscriptions ADD COLUMN cmd TEXT"),
        ("python", "ALTER TABLE subscriptions ADD COLUMN python TEXT"),
        ("code", "ALTER TABLE subscriptions ADD COLUMN code TEXT"),
    ] {
        add_column_if_missing(conn, "subscriptions", column, ddl)?;
    }
    backfill_wire_items(conn)?;
    crate::live_compact::migrate(conn)?;
    conn.execute_batch(
        "CREATE TRIGGER IF NOT EXISTS session_context_reset
         AFTER UPDATE OF context_tokens ON sessions
         WHEN NEW.context_tokens = 0 AND OLD.context_tokens <> 0
         BEGIN
             UPDATE sessions
             SET context_generation = context_generation + 1
             WHERE id = NEW.id;
         END;",
    )?;
    Ok(())
}

/// Turns a home's handover notes into the messages they are now, and drops the column.
///
/// # Invariant
///
/// Appended, never placed ahead of rows already sent; and in one transaction, because a
/// start that stopped between the insert and the drop would append the lesson twice.
/// See `docs/notes/server-handover.md`.
fn settle_handover(conn: &rusqlite::Connection) -> rusqlite::Result<()> {
    if !has_column(conn, "sessions", "handover") {
        return Ok(());
    }
    let tx = conn.unchecked_transaction()?;
    let notes: Vec<(i64, String)> = {
        let mut statement = tx.prepare(
            "SELECT id, handover FROM sessions
              WHERE handover IS NOT NULL AND TRIM(handover) <> ''",
        )?;
        let rows = statement.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?;
        rows.collect::<Result<_, _>>()?
    };
    for (session_id, note) in notes {
        let value = json!({"role": "user", "text": note});
        tx.execute(
            "INSERT INTO messages (session_id, seq, kind, content, display, call_id, created_at)
             VALUES (?1, (SELECT COALESCE(MAX(seq), 0) + 1 FROM messages WHERE session_id = ?1),
                     'text', ?2, ?3, NULL, ?4)",
            params![
                session_id,
                wire_item("text", &value).map(|item| item.to_string()),
                value.to_string(),
                now_millis()
            ],
        )?;
    }
    tx.execute("ALTER TABLE sessions DROP COLUMN handover", [])?;
    tx.commit()
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
                // Beside the database, because that is the one place that knows
                // where it is. See `docs/notes/server-pictures.md`.
                crate::image::pictures::install(&parent.join("pictures"));
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
            live_compact_enabled: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        })
    }

    /// Enables plan requests and delivery only for an explicit nonzero interval.
    pub fn set_live_compact_enabled(&self, enabled: bool) {
        self.live_compact_enabled
            .store(enabled, std::sync::atomic::Ordering::Relaxed);
    }

    fn live_compact_is_enabled(&self) -> bool {
        self.live_compact_enabled
            .load(std::sync::atomic::Ordering::Relaxed)
    }

    /// The connection every picture and upload row is read and written through.
    ///
    /// # Invariant
    ///
    /// One handle, because `image::uploads` writes the table `pictures` rows are keyed
    /// by and the two have to agree about a copy that was just made.
    pub fn picture_conn(&self) -> Arc<Mutex<Connection>> {
        self.conn.clone()
    }

    fn lock(&self) -> MutexGuard<'_, Connection> {
        // A poisoned mutex is safe to recover: the WAL keeps the database consistent.
        self.conn.lock().unwrap_or_else(|e| e.into_inner())
    }

    // -- sessions ----------------------------------------------------------

    /// Creates a session on `model`, which is `provider/model_name` whenever the
    /// roster can place the name.
    ///
    /// # Invariant
    ///
    /// Both model columns are written, and `model_name` is the same model without
    /// its provider. A renamed column keeps its `NOT NULL`, so a row that named
    /// only one of them could not be inserted at all -- and a bare name is right
    /// for a row the roster cannot place.
    /// Creates a session on `remote`, which `None` spells this machine.
    pub fn create_session_on(
        &self,
        name: &str,
        model: &str,
        cwd: &str,
        remote: Option<&str>,
    ) -> anyhow::Result<i64> {
        if name.trim().is_empty() {
            anyhow::bail!("session name must not be empty");
        }
        let now = now_millis();
        let mut conn = self.lock();
        let tx = conn.transaction()?;
        tx.execute(
            "INSERT INTO sessions (name, status, model, model_name, cwd, created_at, updated_at, remote)
             VALUES (?1, 'idle', ?2, ?3, ?4, ?5, ?5, ?6)",
            params![name, model, bare_model(model), cwd, now, remote],
        )
        .map_err(|e| duplicate_session(name, e))?;
        let id = tx.last_insert_rowid();
        tx.commit()?;
        Ok(id)
    }

    /// Copies a session's transcript into a new name, row for row.
    ///
    /// # Invariant
    ///
    /// The two sessions read the same: same rows at the same `seq`, same model,
    /// placement and last measurement. One transaction, so a copy taken while a turn
    /// writes holds every row or none. The target must be a free name and the source
    /// must exist. See `docs/notes/server-store.md`.
    pub fn clone_session(&self, source: &str, name: &str) -> anyhow::Result<CloneReport> {
        if name.trim().is_empty() {
            anyhow::bail!("session name must not be empty");
        }
        let mut conn = self.lock();
        let tx = conn.transaction()?;
        let now = now_millis();
        // `status` says what the session is doing now; the copy is doing nothing.
        let copied = tx
            .execute(
                "INSERT INTO sessions (name, status, model, model_name, cwd, created_at,
                                       updated_at, context_limit, thinking_effort,
                                       context_tokens, context_generation, cached_tokens,
                                       tps, ttft_ms, remote, monitor_seq, monitor_attempts,
                                       cache_tier, cache_keepalive)
                 SELECT ?2, 'idle', model, model_name, cwd, ?3, ?3, context_limit,
                        thinking_effort, context_tokens, context_generation, cached_tokens,
                        tps, ttft_ms, remote, monitor_seq, monitor_attempts, cache_tier,
                        cache_keepalive
                   FROM sessions WHERE name = ?1",
                params![source, name, now],
            )
            .map_err(|e| duplicate_session(name, e))?;
        // No row means the source is not there; the transaction drops unwritten.
        if copied == 0 {
            anyhow::bail!("no session named {source:?} to clone");
        }
        let id = tx.last_insert_rowid();
        let rows = tx.execute(
            "INSERT INTO messages (session_id, seq, kind, content, display, call_id, created_at)
             SELECT ?2, seq, kind, content, display, call_id, created_at
               FROM messages WHERE session_id = (SELECT id FROM sessions WHERE name = ?1)
               ORDER BY seq",
            params![source, id],
        )?;
        let last_seq = tx.query_row(
            "SELECT COALESCE(MAX(seq), 0) FROM messages WHERE session_id = ?1",
            params![id],
            |r| r.get(0),
        )?;
        tx.commit()?;
        Ok(CloneReport { id, rows, last_seq })
    }

    /// Keeps a row's directory equal to where an endpoint put the session's shell.
    ///
    /// # Invariant
    ///
    /// Only an endpoint session writes this: a local row is the directory the
    /// session was made in. See `docs/notes/server-remote.md`.
    /// Records where a session's shells now run, after a move.
    ///
    /// # Invariant
    ///
    /// Both halves in one statement: a row naming the new machine with the old
    /// machine's directory would hand the next start a path that cannot exist there.
    pub fn set_session_placement(
        &self,
        name: &str,
        cwd: &str,
        remote: Option<&str>,
    ) -> anyhow::Result<()> {
        let conn = self.lock();
        conn.execute(
            "UPDATE sessions SET cwd = ?1, remote = ?2 WHERE name = ?3",
            params![cwd, remote, name],
        )?;
        Ok(())
    }

    pub fn set_session_cwd(&self, name: &str, cwd: &str) -> anyhow::Result<()> {
        let conn = self.lock();
        conn.execute(
            "UPDATE sessions SET cwd = ?1 WHERE name = ?2 AND cwd IS NOT ?1",
            params![cwd, name],
        )?;
        Ok(())
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

    /// Every session, the one whose turn ended last first.
    ///
    /// # Invariant
    ///
    /// The key is `ended_at`, or `created_at` for a session whose turn never
    /// ended, newest first. `name` breaks the tie and is unique, so the order is
    /// total, and the client sorts by the same two numbers. `updated_at` is not
    /// part of it: `set_session_tuning` rewrites that column, so ordering by it
    /// moved a row whenever a model was chosen. See `docs/notes/server-store.md`.
    pub fn list_sessions(&self) -> anyhow::Result<Vec<SessionRow>> {
        let conn = self.lock();
        let mut stmt = conn.prepare(&format!(
            "{} ORDER BY COALESCE(s.ended_at, s.created_at) DESC, s.name DESC",
            Self::SESSION_SELECT
        ))?;
        let rows = stmt
            .query_map([], Self::session_row)?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// The columns every session lookup returns, so the list and the single lookup
    /// cannot drift apart.
    ///
    /// # Invariant
    ///
    /// `model` is the whole model, and a row written before it said that keeps the
    /// bare name in `model_name`: this select reads both, so nothing that was pinned
    /// stops being pinned. A column is appended, never inserted: `session_row` reads
    /// these by position.
    const SESSION_SELECT: &'static str = "SELECT s.id, s.name, s.status,
                    COALESCE(s.model, s.model_name), s.cwd,
                    s.created_at, s.updated_at,
                    (SELECT COUNT(*) FROM messages m WHERE m.session_id = s.id) AS messages,
                    s.context_limit, s.thinking_effort,
                    s.context_tokens, s.cached_tokens, s.tps, s.ttft_ms, s.remote,
                    s.cache_tier, s.cache_keepalive, s.ended_at
             FROM sessions s";

    /// Every session in `error`, with what a monitor turn still owes it.
    ///
    /// # Invariant
    ///
    /// The status is the trigger and the column is the memory, so a failure is
    /// reported once and once only, across restarts: `monitor_seq` is the last
    /// row a monitor turn was given, and a turn writes its user row before it
    /// starts, so a later `last_seq` is a failure nobody has been told about.
    /// See `docs/notes/server-monitor.md`.
    pub fn failures(&self) -> anyhow::Result<Vec<Failure>> {
        let conn = self.lock();
        let mut stmt = conn.prepare(
            "SELECT s.id, s.name, COALESCE(s.model, s.model_name),
                    s.monitor_seq, s.monitor_attempts,
                    COALESCE((SELECT MAX(m.seq) FROM messages m WHERE m.session_id = s.id), 0)
             FROM sessions s WHERE s.status = 'error' ORDER BY s.id",
        )?;
        let rows = stmt
            .query_map([], |r| {
                Ok(Failure {
                    id: r.get(0)?,
                    name: r.get(1)?,
                    model: r.get(2)?,
                    monitor_seq: r.get(3)?,
                    monitor_attempts: r.get::<_, i64>(4)?.max(0) as u32,
                    last_seq: r.get(5)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// Records that a monitor turn has had this session's transcript, up to `seq`.
    ///
    /// # Invariant
    ///
    /// Written after the turn is handed over, never before: a crash between the
    /// two repeats a report, which is recoverable, where the other order loses one.
    pub fn mark_monitored(&self, session_id: i64, seq: i64, attempts: u32) -> anyhow::Result<()> {
        let conn = self.lock();
        conn.execute(
            "UPDATE sessions SET monitor_seq = ?2, monitor_attempts = ?3 WHERE id = ?1",
            params![session_id, seq, i64::from(attempts)],
        )?;
        Ok(())
    }

    /// One monitor kind's own memory, or `0` when it has none.
    ///
    /// # Invariant
    ///
    /// Written after the thing it records is accepted, never before: a restart
    /// between the two repeats a handover, which is recoverable, where the other
    /// order loses one. See `docs/notes/server-monitor.md`.
    pub fn monitor_value(&self, kind: &str) -> anyhow::Result<i64> {
        let conn = self.lock();
        Ok(conn
            .query_row(
                "SELECT value FROM monitor_state WHERE kind = ?1",
                params![kind],
                |r| r.get(0),
            )
            .optional()?
            .unwrap_or(0))
    }

    pub fn set_monitor_value(&self, kind: &str, value: i64) -> anyhow::Result<()> {
        let conn = self.lock();
        conn.execute(
            "INSERT INTO monitor_state (kind, value) VALUES (?1, ?2)
             ON CONFLICT(kind) DO UPDATE SET value = ?2",
            params![kind, value],
        )?;
        Ok(())
    }

    /// How many models this session has been through for its current failure.
    pub fn set_monitor_attempts(&self, session_id: i64, attempts: u32) -> anyhow::Result<()> {
        let conn = self.lock();
        conn.execute(
            "UPDATE sessions SET monitor_attempts = ?2 WHERE id = ?1",
            params![session_id, i64::from(attempts)],
        )?;
        Ok(())
    }

    /// The newest row of `session_id`, as the database has it.
    pub fn last_message_seq(&self, session_id: i64) -> anyhow::Result<i64> {
        let conn = self.lock();
        Ok(conn.query_row(
            "SELECT COALESCE((SELECT MAX(seq) FROM messages WHERE session_id = ?1), 0)",
            params![session_id],
            |r| r.get(0),
        )?)
    }

    /// A session's name, by row id: what a log row carries, so a reader need not join.
    pub fn name_of_session(&self, id: i64) -> anyhow::Result<Option<String>> {
        let conn = self.lock();
        let mut stmt = conn.prepare("SELECT name FROM sessions WHERE id = ?1")?;
        Ok(stmt.query_row([id], |r| r.get(0)).optional()?)
    }

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
            context_limit: r.get::<_, Option<i64>>(8)?.map(|v| v as u8),
            thinking_effort: r.get(9)?,
            usage: protocol::Usage {
                context_tokens: r.get::<_, i64>(10)?.max(0) as u64,
                cached_tokens: r.get::<_, Option<i64>>(11)?.map(|n| n.max(0) as u64),
                tps: r.get(12)?,
                ttft_ms: r.get::<_, Option<i64>>(13)?.map(|ms| ms.max(0) as u64),
            },
            remote: r.get(14)?,
            cache_tier: r.get(15)?,
            cache_keepalive: r.get::<_, i64>(16)? != 0,
            ended_at: r.get(17)?,
        })
    }

    /// Records what a session was told to use: the model and the tuning beside it.
    ///
    /// # Invariant
    ///
    /// All of it in one statement, so no reader sees half-applied tuning. `model`
    /// is `provider/model_name`, and `None` puts the session back on the server's
    /// own model, whose name only the caller knows.
    pub fn set_session_tuning(
        &self,
        id: i64,
        model: Option<&str>,
        context_limit: Option<u8>,
        thinking_effort: Option<&str>,
        default_model: &str,
    ) -> anyhow::Result<()> {
        self.lock().execute(
            "UPDATE sessions
                SET model = COALESCE(?1, ?2),
                    context_limit = ?3,
                    thinking_effort = ?4,
                    updated_at = ?5
              WHERE id = ?6",
            params![
                model,
                default_model,
                context_limit.map(|v| v as i64),
                thinking_effort,
                now_millis(),
                id
            ],
        )?;
        Ok(())
    }

    /// The session's static tier and heartbeat switch, written apart from tuning.
    pub fn set_session_cache(
        &self,
        id: i64,
        tier: Option<&str>,
        keepalive: bool,
    ) -> anyhow::Result<()> {
        self.lock().execute(
            "UPDATE sessions
                SET cache_tier = ?1,
                    cache_keepalive = ?2,
                    updated_at = ?3
              WHERE id = ?4",
            params![tier, i64::from(keepalive), now_millis(), id],
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

    /// Drops every row after `seq`, returning how many went.
    ///
    /// # Invariant
    ///
    /// Only a failed request attempt's own rows: its window was pinned to `seq`
    /// before the request was sent, so a row beyond it is one that attempt wrote,
    /// and the retry that follows has to send what the first attempt sent. See
    /// `docs/notes/server-store.md`.
    pub fn truncate_after(&self, session_id: i64, seq: i64) -> anyhow::Result<usize> {
        let mut conn = self.lock();
        let tx = conn.transaction()?;
        let discarded = {
            let mut statement = tx.prepare(
                "SELECT id FROM live_compact_checkpoints
                 WHERE session_id = ?1 AND result_seq > ?2",
            )?;
            let rows = statement.query_map(params![session_id, seq], |row| row.get::<_, i64>(0))?;
            rows.collect::<Result<Vec<_>, _>>()?
        };
        for checkpoint in &discarded {
            tx.execute(
                "DELETE FROM live_compact_artifacts WHERE checkpoint_id = ?1",
                params![checkpoint],
            )?;
            tx.execute(
                "DELETE FROM live_compact_checkpoints WHERE id = ?1",
                params![checkpoint],
            )?;
        }
        let count = tx.execute(
            "DELETE FROM messages WHERE session_id = ?1 AND seq > ?2",
            params![session_id, seq],
        )?;
        if !discarded.is_empty() {
            let context_tokens = tx.query_row(
                "SELECT context_tokens FROM sessions WHERE id = ?1",
                params![session_id],
                |row| row.get::<_, i64>(0),
            )?;
            let checkpoints = i64::try_from(discarded.len())?;
            let trigger_advance = if context_tokens == 0 { 0 } else { 1 };
            // Cancel the trigger's conditional +1, leaving one generation rewind per checkpoint.
            tx.execute(
                "UPDATE sessions SET live_compact_baseline_tokens = NULL,
                        live_compact_requested = 0, context_tokens = 0, cached_tokens = NULL,
                        context_generation = context_generation - ?2
                 WHERE id = ?1",
                params![session_id, checkpoints + trigger_advance],
            )?;
        }
        tx.commit()?;
        Ok(count)
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

    /// The session's last answer: the last assistant `text` row.
    ///
    /// # Invariant
    ///
    /// A `text` row is either a user message or the assistant's own words; the
    /// last assistant one is the answer of the turn that just ended, so a
    /// reader that wants the answer does not have to read the whole transcript
    /// to find it.
    pub fn last_answer(&self, session_id: i64) -> anyhow::Result<Option<String>> {
        let conn = self.lock();
        let mut stmt = conn.prepare(
            "SELECT display FROM messages WHERE session_id = ?1 AND kind = 'text' \
             ORDER BY seq DESC LIMIT 8",
        )?;
        let rows = stmt.query_map(params![session_id], |r| r.get::<_, String>(0))?;
        for row in rows {
            let display = row?;
            let value: serde_json::Value =
                serde_json::from_str(&display).unwrap_or(serde_json::Value::Null);
            if value.get("role").and_then(|v| v.as_str()) == Some("assistant")
                && let Some(text) = value.get("text").and_then(|v| v.as_str())
            {
                return Ok(Some(text.to_string()));
            }
        }
        Ok(None)
    }

    /// The provider's last input measurement and the transcript generation it belongs to.
    pub fn context_measurement(&self, session_id: i64) -> anyhow::Result<(u64, u64)> {
        let conn = self.lock();
        let (tokens, generation): (i64, i64) = conn.query_row(
            "SELECT context_tokens, context_generation FROM sessions WHERE id = ?1",
            params![session_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        Ok((tokens.max(0) as u64, generation.max(0) as u64))
    }

    /// Input tokens the provider reported for the last request, or 0 if this
    /// session has not made one yet.
    pub fn context_tokens(&self, session_id: i64) -> anyhow::Result<u64> {
        Ok(self.context_measurement(session_id)?.0)
    }

    /// The transcript generation a context notice belongs to.
    pub fn context_generation(&self, session_id: i64) -> anyhow::Result<u64> {
        Ok(self.context_measurement(session_id)?.1)
    }

    /// What one request measured: input tokens, what of them was cached, the
    /// throughput and the first-token delay.
    ///
    /// # Invariant
    ///
    /// One statement: the four are one measurement, and a window that read the
    /// context of one request beside the rate of another would be a lie the row
    /// cannot explain.
    pub fn set_usage_for_generation(
        &self,
        session_id: i64,
        generation: u64,
        usage: protocol::Usage,
    ) -> anyhow::Result<()> {
        self.lock().execute(
            "UPDATE sessions SET context_tokens = ?1, cached_tokens = ?2, tps = ?3, \
             ttft_ms = ?4 WHERE id = ?5 AND context_generation = ?6",
            params![
                usage.context_tokens as i64,
                usage.cached_tokens.map(|n| n as i64),
                usage.tps,
                usage.ttft_ms.map(|ms| ms as i64),
                session_id,
                generation as i64
            ],
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

    // -- what a stream has said so far -------------------------------------

    /// Records the text of an item still being streamed.
    ///
    /// # Invariant
    ///
    /// Overwritten, not appended: this is a draft, and the row it becomes is written
    /// once when the item finishes (or at the next start, if it never does). The
    /// transcript is append-only; this table is not part of it.
    pub fn save_draft(
        &self,
        session_id: i64,
        item_id: &str,
        turn: u64,
        part: &str,
        text: &str,
    ) -> anyhow::Result<()> {
        let conn = self.lock();
        conn.execute(
            "INSERT INTO drafts (session_id, item_id, turn, part, text, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT(session_id, item_id) DO UPDATE SET
                 text = excluded.text,
                 part = excluded.part,
                 turn = excluded.turn,
                 updated_at = excluded.updated_at",
            params![session_id, item_id, turn as i64, part, text, now_millis()],
        )?;
        Ok(())
    }

    /// Forgets an item's draft: its finished row is the record now.
    pub fn drop_draft(&self, session_id: i64, item_id: &str) -> anyhow::Result<()> {
        let conn = self.lock();
        conn.execute(
            "DELETE FROM drafts WHERE session_id = ?1 AND item_id = ?2",
            params![session_id, item_id],
        )?;
        Ok(())
    }

    /// Forgets every draft of `session`: a new step's stream is new items.
    pub fn drop_drafts(&self, session_id: i64) -> anyhow::Result<usize> {
        let conn = self.lock();
        Ok(conn.execute(
            "DELETE FROM drafts WHERE session_id = ?1",
            params![session_id],
        )?)
    }

    /// Appends a row for every draft a stopped process left behind, and clears them.
    ///
    /// # Invariant
    ///
    /// The text is stored as it stood, `interrupted: true`, and in the order the
    /// drafts were last touched. Half a message is what the reader was shown, and a
    /// transcript that dropped it would say the model said nothing at all.
    pub fn settle_drafts(&self, session_id: i64) -> anyhow::Result<usize> {
        let pending: Vec<(String, u64, String, String)> = {
            let conn = self.lock();
            let mut stmt = conn.prepare(
                "SELECT item_id, turn, part, text FROM drafts
                  WHERE session_id = ?1 AND text <> ''
                  ORDER BY updated_at, item_id",
            )?;
            let rows = stmt.query_map(params![session_id], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, i64>(1)? as u64,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                ))
            })?;
            let mut out = Vec::new();
            for row in rows {
                out.push(row?);
            }
            out
        };
        let mut settled = 0;
        for (item_id, turn, part, text) in pending {
            // Appended last; see `docs/notes/server-store.md`.
            let stored = match part.as_str() {
                "output" => Some((
                    "text",
                    json!({"role": "assistant", "text": text, "interrupted": true,
                           "item_id": item_id, "turn": turn}),
                )),
                "reasoning" => Some((
                    "reasoning",
                    json!({"summary": text, "item_id": item_id,
                           "interrupted": true, "turn": turn}),
                )),
                // A call's text is its arguments, which the call row carries; an
                // activity is progress, never an answer.
                _ => None,
            };
            if let Some((kind, payload)) = stored {
                self.append_message(session_id, kind, &payload)?;
                settled += 1;
            }
        }
        self.drop_drafts(session_id)?;
        Ok(settled)
    }

    /// Every call in `session` whose answer a request would look for and not find.
    ///
    /// # Invariant
    ///
    /// Read from the rows themselves, by `call_id`, because that is what a provider
    /// pairs on. A call with no result is a transcript a provider rejects, and it is
    /// only ever left by a run that died inside a tool: a turn writes its own answer
    /// before it ends. See `docs/notes/server-store.md`.
    pub fn open_calls(&self, session_id: i64) -> anyhow::Result<Vec<(String, String, u32)>> {
        let conn = self.lock();
        let mut stmt = conn.prepare(
            "SELECT c.call_id, c.display FROM messages c
              WHERE c.session_id = ?1 AND c.kind = 'tool_call' AND c.call_id IS NOT NULL
                AND NOT EXISTS (
                    SELECT 1 FROM messages r
                     WHERE r.session_id = c.session_id AND r.kind = 'tool_result'
                       AND r.call_id = c.call_id)
              ORDER BY c.seq",
        )?;
        let rows = stmt.query_map(params![session_id], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?;
        let mut out = Vec::new();
        for row in rows {
            let (call_id, display) = row?;
            let value: serde_json::Value = serde_json::from_str(&display).unwrap_or_default();
            let name = value
                .get("name")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string();
            let step = value
                .get("step")
                .and_then(|v| v.as_u64())
                .unwrap_or_default() as u32;
            out.push((call_id, name, step));
        }
        Ok(out)
    }

    /// Answers a call nothing answered: what the tool that died is said to have done.
    ///
    /// # Invariant
    ///
    /// Appended, never rewritten, and written once: a second pass finds the result it
    /// wrote and leaves the row alone. That is what makes this safe to run at every
    /// start rather than only after a crash the last run knew about.
    pub fn answer_open_calls(&self, session_id: i64) -> anyhow::Result<usize> {
        let open = self.open_calls(session_id)?;
        for (call_id, name, step) in &open {
            self.append_message(
                session_id,
                "tool_result",
                &json!({
                    "call_id": call_id,
                    "output": interrupted_call(),
                    "images": [],
                    "step": step,
                    "ok": false,
                    "elapsed_ms": 0,
                    "tool": name,
                }),
            )?;
            self.finish_tool_call(session_id, call_id, &interrupted_call(), "cancelled")?;
        }
        Ok(open.len())
    }

    /// The text of every message waiting in the inbox, oldest first.
    pub fn queued_texts(&self, session: &str) -> anyhow::Result<Vec<String>> {
        let conn = self.lock();
        let mut stmt = conn.prepare(
            "SELECT payload FROM pending_tasks WHERE session = ?1 AND kind = 'inject' ORDER BY id",
        )?;
        let rows = stmt.query_map(params![session], |row| row.get::<_, String>(0))?;
        let mut out = Vec::new();
        for row in rows {
            let value: serde_json::Value = serde_json::from_str(&row?).unwrap_or_default();
            if let Some(text) = value.get("message").and_then(|m| m.as_str()) {
                out.push(text.to_string());
            }
        }
        Ok(out)
    }

    /// Adds one measurement: `chars` written, `tokens` the provider counted for it.
    pub fn note_token_ratio(&self, model: &str, chars: u64, tokens: u64) -> anyhow::Result<()> {
        self.lock().execute(
            "INSERT INTO token_ratio (model, chars, tokens) VALUES (?1, ?2, ?3)
             ON CONFLICT(model) DO UPDATE SET chars = chars + excluded.chars,
                                              tokens = tokens + excluded.tokens",
            params![model, chars as i64, tokens as i64],
        )?;
        Ok(())
    }

    /// How many tokens a character is worth on `model`, from what was measured.
    ///
    /// # Invariant
    ///
    /// A model nobody has measured falls back to [`CHARS_PER_TOKEN`], which claims
    /// nothing about this provider: it only has to be close enough that a plan is not
    /// made of zeros.
    pub fn token_ratio(&self, model: &str) -> f64 {
        let row: Option<(i64, i64)> = self
            .lock()
            .query_row(
                "SELECT chars, tokens FROM token_ratio WHERE model = ?1",
                params![model],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()
            .unwrap_or(None);
        match row {
            Some((chars, tokens)) if chars > 0 && tokens > 0 => tokens as f64 / chars as f64,
            _ => 1.0 / CHARS_PER_TOKEN,
        }
    }

    /// The orderings `owner` has already emitted, as `prefix_cache` records them.
    ///
    /// # Invariant
    ///
    /// `owner` is the unit whose requests share bytes -- a session's directory -- not a
    /// session: two sessions sending the same head must plan from the same orderings or
    /// their heads differ. See `docs/notes/server-prefix-cache.md`.
    pub fn prefix_layout(
        &self,
        owner: &str,
    ) -> anyhow::Result<Option<crate::prefix_cache::Layout>> {
        let row: Option<String> = {
            let conn = self.lock();
            conn.query_row(
                "SELECT layout FROM prefix_layout WHERE owner = ?1",
                params![owner],
                |row| row.get(0),
            )
            .optional()?
        };
        let Some(text) = row else { return Ok(None) };
        match serde_json::from_str::<crate::prefix_cache::Layout>(&text) {
            // Both lookups are derived: reading the row is what rebuilds them.
            Ok(mut layout) => {
                layout.reindex();
                Ok(Some(layout))
            }
            Err(e) => {
                tracing::warn!(error = %e, "the stored prefix layout is not readable");
                Ok(None)
            }
        }
    }

    /// Replaces what is stored, so the next start plans from what this one learnt.
    pub fn save_prefix_layout(
        &self,
        owner: &str,
        layout: &crate::prefix_cache::Layout,
    ) -> anyhow::Result<()> {
        let text = serde_json::to_string(layout)?;
        self.lock().execute(
            "INSERT INTO prefix_layout (owner, layout) VALUES (?1, ?2)
             ON CONFLICT(owner) DO UPDATE SET layout = excluded.layout",
            params![owner, text],
        )?;
        Ok(())
    }

    /// Every operator message this session has stored, oldest first. Harness
    /// notices are not operator messages.
    pub fn user_texts(&self, session_id: i64) -> anyhow::Result<Vec<String>> {
        let conn = self.lock();
        let mut stmt = conn.prepare(
            "SELECT display FROM messages WHERE session_id = ?1 AND kind = 'text' ORDER BY seq",
        )?;
        let rows = stmt.query_map(params![session_id], |row| row.get::<_, String>(0))?;
        let mut out = Vec::new();
        for row in rows {
            let value: serde_json::Value = serde_json::from_str(&row?).unwrap_or_default();
            if value.get("role").and_then(|r| r.as_str()) != Some("user") {
                continue;
            }
            if value.get("harness").is_some() {
                continue;
            }
            if let Some(text) = value.get("text").and_then(|t| t.as_str()) {
                out.push(text.to_string());
            }
        }
        Ok(out)
    }

    /// The provider measurement and durable request state used at a step boundary.
    pub fn live_compact_measurement(
        &self,
        session_id: i64,
    ) -> anyhow::Result<crate::live_compact::Measurement> {
        let conn = self.lock();
        let row = conn.query_row(
            "SELECT context_tokens, live_compact_baseline_tokens,
                    live_compact_requested, context_generation
             FROM sessions WHERE id = ?1",
            params![session_id],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, Option<i64>>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, i64>(3)?,
                ))
            },
        )?;
        Ok(crate::live_compact::Measurement {
            tokens: row.0.max(0) as u64,
            baseline_tokens: row.1.map(|tokens| tokens.max(0) as u64),
            requested: row.2 != 0,
            generation: row.3.max(0) as u64,
        })
    }

    /// Records the first provider measurement for the current visible checkpoint.
    pub fn set_live_compact_baseline(
        &self,
        session_id: i64,
        generation: u64,
        input_tokens: u64,
    ) -> anyhow::Result<()> {
        self.lock().execute(
            "UPDATE sessions SET live_compact_baseline_tokens = ?1
             WHERE id = ?2 AND context_generation = ?3
               AND live_compact_origin_seq IS NOT NULL
               AND live_compact_baseline_tokens IS NULL
               AND live_compact_started = 1",
            params![input_tokens as i64, session_id, generation as i64],
        )?;
        Ok(())
    }

    /// Marks the request and its durable injection together, so a restart cannot nag twice.
    pub fn request_live_compact(
        &self,
        session_id: i64,
        session: &str,
        generation: u64,
        message: &str,
    ) -> anyhow::Result<bool> {
        let mut conn = self.lock();
        let tx = conn.transaction()?;
        let marked = tx.execute(
            "UPDATE sessions SET live_compact_requested = 1
             WHERE id = ?1 AND context_generation = ?2
               AND live_compact_origin_seq IS NOT NULL
               AND live_compact_baseline_tokens IS NOT NULL
               AND live_compact_requested = 0",
            params![session_id, generation as i64],
        )?;
        if marked == 0 {
            return Ok(false);
        }
        tx.execute(
            "INSERT INTO pending_tasks (session, kind, payload, created_at)
             VALUES (?1, 'inject', ?2, ?3)",
            params![
                session,
                serde_json::to_string(&json!({
                    "session": session,
                    "message": message,
                    "live_compact": {"generation": generation},
                }))?,
                now_millis()
            ],
        )?;
        tx.commit()?;
        Ok(true)
    }

    /// Sets the starting checkpoint baseline at a turn end; a usage may only
    /// re-establish one after this has run.
    pub fn establish_live_compact_baseline(&self, session_id: i64) -> anyhow::Result<()> {
        self.lock().execute(
            "UPDATE sessions SET live_compact_baseline_tokens = context_tokens,
                    live_compact_started = 1
             WHERE id = ?1 AND live_compact_origin_seq IS NOT NULL
               AND live_compact_baseline_tokens IS NULL AND context_tokens > 0",
            params![session_id],
        )?;
        Ok(())
    }

    /// Lets a failed or stale summary request be asked again without deleting its marker.
    pub fn reset_live_compact_request(&self, session_id: i64) -> anyhow::Result<()> {
        self.lock().execute(
            "UPDATE sessions SET live_compact_requested = 0 WHERE id = ?1",
            params![session_id],
        )?;
        Ok(())
    }

    /// Persists the summary and its complete artifact set in the checkpoint's transaction.
    pub fn append_live_compact_result(
        &self,
        session_id: i64,
        call_id: &str,
        content: &serde_json::Value,
    ) -> anyhow::Result<crate::live_compact::Committed> {
        let mut conn = self.lock();
        let tx = conn.transaction()?;
        let request = tx
            .query_row(
                "SELECT display FROM messages
                 WHERE session_id = ?1 AND kind = ?2
                 ORDER BY seq DESC LIMIT 1",
                params![session_id, crate::live_compact::REQUEST_KIND],
                |row| row.get::<_, String>(0),
            )
            .optional()?;
        let request = request
            .map(|display| serde_json::from_str::<crate::live_compact::Request>(&display))
            .transpose()?
            .ok_or_else(|| anyhow::anyhow!("live_compact has no durable request"))?;
        let (call_seq, call_display) = tx.query_row(
            "SELECT seq, display FROM messages
             WHERE session_id = ?1 AND kind = 'tool_call' AND call_id = ?2",
            params![session_id, call_id],
            |row| Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?)),
        )?;
        let call: serde_json::Value = serde_json::from_str(&call_display)?;
        if call.get("name").and_then(serde_json::Value::as_str)
            != Some(crate::tools::live_compact::LIVE_COMPACT_TOOL)
            || call_seq <= request.request_seq
        {
            anyhow::bail!("live_compact call does not belong to the latest request");
        }
        let (current_generation, context_tokens) = tx.query_row(
            "SELECT context_generation, context_tokens FROM sessions WHERE id = ?1",
            params![session_id],
            |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)),
        )?;
        if current_generation.max(0) as u64 != request.generation {
            anyhow::bail!("live_compact request belongs to an older context generation");
        }

        let mut statement = tx.prepare(
            "SELECT seq FROM messages
             WHERE session_id = ?1 AND kind = 'reasoning'
               AND seq > ?2 AND seq < ?3 ORDER BY seq",
        )?;
        let rows = statement
            .query_map(params![session_id, request.request_seq, call_seq], |row| {
                row.get::<_, i64>(0)
            })?;
        let mut artifacts = rows.collect::<Result<Vec<_>, _>>()?;
        drop(statement);
        artifacts.push(call_seq);

        let result_seq = Self::append_message_conn(&tx, session_id, "tool_result", content)?;
        tx.execute(
            "INSERT INTO live_compact_checkpoints
             (session_id, segment_after_seq, request_seq, round_start_seq,
              cutoff_seq, result_seq, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                session_id,
                request.segment_after_seq,
                request.request_seq,
                request.round_start_seq,
                request.cutoff_seq,
                result_seq,
                now_millis()
            ],
        )?;
        let checkpoint_id = tx.last_insert_rowid();
        artifacts.push(result_seq);
        for message_seq in artifacts {
            tx.execute(
                "INSERT INTO live_compact_artifacts (session_id, message_seq, checkpoint_id)
                 VALUES (?1, ?2, ?3)",
                params![session_id, message_seq, checkpoint_id],
            )?;
        }

        // Nonzero resets trigger +1; zero needs an explicit advance so old usage stays stale.
        tx.execute(
            "UPDATE sessions SET live_compact_baseline_tokens = NULL,
                    live_compact_requested = 0, context_tokens = 0, cached_tokens = NULL
             WHERE id = ?1",
            params![session_id],
        )?;
        if context_tokens == 0 {
            tx.execute(
                "UPDATE sessions SET context_generation = context_generation + 1 WHERE id = ?1",
                params![session_id],
            )?;
        }
        tx.commit()?;
        Ok(crate::live_compact::Committed {
            checkpoint_id,
            result_seq,
            generation: request.generation + 1,
        })
    }

    /// The transcript a request reads, closed at the row that exists right now.
    pub fn request_source(&self, session_id: i64) -> anyhow::Result<crate::llm::RequestSource> {
        Ok(crate::llm::RequestSource {
            db_path: self.path.clone(),
            session_id,
            upto_seq: self.last_seq(session_id)?,
            store: self.clone(),
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

    /// The event as the log now holds it, or nothing when it could not be written.
    ///
    /// # Invariant
    ///
    /// A caller that wants to *announce* a row needs the row's own id and stamp,
    /// not a guess: the id is what a subscriber resumes from, and a number
    /// invented here would be a cursor into the wrong place.
    /// The chance a warm prefix was gone, measured from this home's own answers.
    ///
    /// # Invariant
    ///
    /// A row counts only when it carries a hit ratio against a non-zero expected
    /// prefix: a first request has nothing to hit, so it says nothing about a
    /// provider dropping a cache. `None` is no such row yet.
    pub fn cache_hit_posterior(&self, model: &str) -> anyhow::Result<Option<f64>> {
        let conn = self.lock();
        let mut statement = conn.prepare(
            "SELECT AVG(1.0 - json_extract(payload, '$.hit_ratio')) FROM events \
             WHERE kind = 'usage' AND json_extract(payload, '$.model') = ?1 \
             AND json_extract(payload, '$.hit_ratio') IS NOT NULL \
             AND json_extract(payload, '$.expected') > 0",
        )?;
        Ok(statement.query_row(params![model], |row| row.get::<_, Option<f64>>(0))?)
    }

    /// When this session last completed, in millis. See `server-prefix-cache.md`.
    pub fn last_usage_at(&self, session_id: i64) -> anyhow::Result<Option<i64>> {
        let conn = self.lock();
        let mut statement = conn.prepare(
            "SELECT MAX(created_at) FROM events WHERE session_id = ?1 AND kind = 'usage'",
        )?;
        Ok(statement.query_row(params![session_id], |row| row.get::<_, Option<i64>>(0))?)
    }

    pub fn append_event_row(
        &self,
        session_id: Option<i64>,
        kind: &str,
        payload: &serde_json::Value,
    ) -> anyhow::Result<EventRow> {
        self.append_event_inner(session_id, kind, payload)
    }

    fn append_event_inner(
        &self,
        session_id: Option<i64>,
        kind: &str,
        payload: &serde_json::Value,
    ) -> anyhow::Result<EventRow> {
        let conn = self.lock();
        let seq: i64 = conn
            .query_row("SELECT COALESCE(MAX(seq), 0) + 1 FROM events", [], |r| {
                r.get(0)
            })
            .unwrap_or(1);
        let at = now_millis();
        let payload = serde_json::to_string(payload)?;
        conn.execute(
            "INSERT INTO events (session_id, seq, kind, payload, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![session_id, seq, kind, payload.clone(), at],
        )?;
        Ok(EventRow {
            id: conn.last_insert_rowid(),
            seq,
            session_id,
            kind: kind.to_string(),
            payload,
            created_at: at,
        })
    }

    /// Log rows after a cursor, oldest first, as the log keeps them.
    ///
    /// # Invariant
    ///
    /// `after` is exclusive and is the log's `id`, never `seq`: the id is monotonic over
    /// the table where `seq` is per session, and a filter is applied on the way out.
    /// One event row, by its id: what a retry re-reads.
    pub fn event_row(&self, id: i64) -> anyhow::Result<Option<EventRow>> {
        let conn = self.lock();
        let mut stmt = conn.prepare(
            "SELECT id, seq, session_id, kind, payload, created_at FROM events WHERE id = ?1",
        )?;
        Ok(stmt
            .query_row(params![id], |r| {
                Ok(EventRow {
                    id: r.get(0)?,
                    seq: r.get(1)?,
                    session_id: r.get(2)?,
                    kind: r.get(3)?,
                    payload: r.get(4)?,
                    created_at: r.get(5)?,
                })
            })
            .optional()?)
    }

    pub fn events_after(
        &self,
        after: i64,
        kind: Option<&str>,
        session: Option<i64>,
        limit: u32,
    ) -> anyhow::Result<Vec<EventRow>> {
        let conn = self.lock();
        let mut stmt = conn.prepare(
            "SELECT id, seq, session_id, kind, payload, created_at FROM events
             WHERE id > ?1
               AND (?2 IS NULL OR kind = ?2)
               AND (?3 IS NULL OR session_id = ?3)
             ORDER BY id ASC
             LIMIT ?4",
        )?;
        let rows = stmt
            .query_map(params![after, kind, session, limit], |r| {
                Ok(EventRow {
                    id: r.get(0)?,
                    seq: r.get(1)?,
                    session_id: r.get(2)?,
                    kind: r.get(3)?,
                    payload: r.get(4)?,
                    created_at: r.get(5)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
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

    /// The boundary every user-role row lands on, wherever it arrived from.
    ///
    /// # Invariant
    ///
    /// Heir empties a transcript from under a still-running turn, keeping only
    /// the in-flight call and its result, and its lesson lands as an injection —
    /// so "fresh" means no `text` row survives, never a physically empty table,
    /// and the reset and plan request live here rather than on `begin_turn`.
    fn append_user_conn(
        &self,
        tx: &rusqlite::Transaction<'_>,
        session_id: i64,
        expanded: &crate::meta::Expanded,
    ) -> anyhow::Result<i64> {
        let no_text = tx.query_row(
            "SELECT NOT EXISTS (SELECT 1 FROM messages
                 WHERE session_id = ?1 AND kind = 'text')",
            params![session_id],
            |row| row.get::<_, bool>(0),
        )?;
        if no_text {
            // No text row survives: whatever stood before (heir keeps only an
            // in-flight pair) is not this lineage's history.
            tx.execute(
                "DELETE FROM live_compact_artifacts WHERE session_id = ?1",
                params![session_id],
            )?;
            tx.execute(
                "DELETE FROM live_compact_checkpoints WHERE session_id = ?1",
                params![session_id],
            )?;
            tx.execute(
                "UPDATE sessions SET live_compact_origin_seq = NULL,
                        live_compact_baseline_tokens = NULL, live_compact_requested = 0,
                        live_compact_started = 0
                 WHERE id = ?1",
                params![session_id],
            )?;
        }
        let origin_was_null = tx.query_row(
            "SELECT live_compact_origin_seq IS NULL FROM sessions WHERE id = ?1",
            params![session_id],
            |row| row.get::<_, bool>(0),
        )?;
        let seq = Self::append_message_conn(tx, session_id, "text", &user_row(expanded))?;
        tx.execute(
            "UPDATE sessions SET live_compact_origin_seq = ?2,
                    live_compact_baseline_tokens = NULL
             WHERE id = ?1 AND live_compact_origin_seq IS NULL",
            params![session_id, seq],
        )?;
        if origin_was_null && self.live_compact_is_enabled() {
            // SPEC: the first true user message is asked for a plan, one row behind it.
            return Self::append_message_conn(
                tx,
                session_id,
                "text",
                &serde_json::json!({
                    "role": "user",
                    "text": crate::live_compact::PLAN_NOTICE,
                    "harness": "live_compact_plan",
                }),
            );
        }
        Ok(seq)
    }

    /// Accepts an idle session's first message and its turn obligation together.
    ///
    /// # Invariant
    ///
    /// Answers the task and the last message seq: the caller announces where the
    /// transcript now ends, including the plan row. See `docs/notes/server-store.md`.
    pub fn begin_turn(
        &self,
        session_id: i64,
        session: &str,
        expanded: &crate::meta::Expanded,
    ) -> anyhow::Result<(i64, i64)> {
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
        let seq = self.append_user_conn(&tx, session_id, expanded)?;
        tx.commit()?;
        Ok((id, seq))
    }

    /// The newest copy this home holds for `path`, and the type it was read as.
    ///
    /// # Invariant
    ///
    /// The newest row answers, not the oldest: a rewritten file mints a new copy, so a
    /// lookup that walked back would offer a picture nothing was sent with.
    pub fn picture_of(&self, path: &str) -> Option<(i64, String)> {
        let conn = self.lock();
        conn.query_row(
            "SELECT id, media_type FROM pictures WHERE path = ?1 ORDER BY id DESC LIMIT 1",
            params![path],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()
        .ok()?
    }

    /// Records one picture copy, returning the id a message names it by.
    pub fn add_picture(&self, path: &str, media_type: &str) -> anyhow::Result<i64> {
        let mut conn = self.lock();
        let tx = conn.transaction()?;
        tx.execute(
            "INSERT INTO pictures (path, media_type) VALUES (?1, ?2)",
            params![path, media_type],
        )?;
        let id = tx.last_insert_rowid();
        tx.commit()?;
        Ok(id)
    }

    /// The media type one copy was stored with: an id's own row, not the path's newest.
    pub fn picture_media_type(&self, id: i64) -> Option<String> {
        let conn = self.lock();
        conn.query_row(
            "SELECT media_type FROM pictures WHERE id = ?1",
            params![id],
            |r| r.get::<_, String>(0),
        )
        .optional()
        .ok()?
    }

    /// Forgets a copy that could not be written, so no row names a missing picture.
    pub fn drop_picture(&self, id: i64) {
        let conn = self.lock();
        if let Err(e) = conn.execute("DELETE FROM pictures WHERE id = ?1", params![id]) {
            tracing::warn!(id, error = %e, "cannot drop a picture copy that was never written");
        }
    }

    /// Delivers the inbox in one transaction: how many, the longest wait, the last seq.
    pub fn deliver_pending(
        &self,
        session_id: i64,
        session: &str,
    ) -> anyhow::Result<(usize, i64, i64)> {
        let mut conn = self.lock();
        let tx = conn.transaction()?;
        let tasks = {
            let mut stmt = tx.prepare(
                "SELECT id, payload, created_at FROM pending_tasks
                 WHERE session = ?1 AND kind = 'inject' ORDER BY id",
            )?;
            let rows = stmt.query_map(params![session], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, i64>(2)?,
                ))
            })?;
            rows.collect::<Result<Vec<_>, _>>()?
        };
        let mut waited = 0;
        let mut delivered = 0;
        // The watermark the caller announces: the last row this transaction wrote.
        let mut last = 0;
        for (id, payload, written_at) in tasks {
            let payload: serde_json::Value = serde_json::from_str(&payload)?;
            if let Some(notice) = payload.get("context_notice") {
                let limit = notice
                    .get("limit")
                    .and_then(serde_json::Value::as_u64)
                    .ok_or_else(|| anyhow::anyhow!("pending inject {id} has no context limit"))?;
                let generation = notice
                    .get("generation")
                    .and_then(serde_json::Value::as_u64)
                    .ok_or_else(|| {
                        anyhow::anyhow!("pending inject {id} has no context generation")
                    })?;
                let (tokens, current_generation): (i64, i64) = tx.query_row(
                    "SELECT context_tokens, context_generation FROM sessions WHERE id = ?1",
                    params![session_id],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )?;
                // A notice is advice about a measurement, not an operator's message.
                if (current_generation.max(0) as u64) != generation
                    || (tokens.max(0) as u64) < limit
                {
                    tx.execute("DELETE FROM pending_tasks WHERE id = ?1", params![id])?;
                    continue;
                }
            }

            if payload.get("live_compact").is_some() && !self.live_compact_is_enabled() {
                tx.execute("DELETE FROM pending_tasks WHERE id = ?1", params![id])?;
                tx.execute(
                    "UPDATE sessions SET live_compact_requested = 0 WHERE id = ?1",
                    params![session_id],
                )?;
                continue;
            }
            let mut compact_round = None;
            if let Some(request) = payload.get("live_compact") {
                let generation = request
                    .get("generation")
                    .and_then(serde_json::Value::as_u64)
                    .ok_or_else(|| {
                        anyhow::anyhow!("pending live compact {id} has no generation")
                    })?;
                let (current_generation, origin_seq) = tx.query_row(
                    "SELECT context_generation, live_compact_origin_seq
                     FROM sessions WHERE id = ?1",
                    params![session_id],
                    |row| Ok((row.get::<_, i64>(0)?, row.get::<_, Option<i64>>(1)?)),
                )?;
                let previous_cutoff = tx
                    .query_row(
                        "SELECT cutoff_seq FROM live_compact_checkpoints
                         WHERE session_id = ?1 ORDER BY id DESC LIMIT 1",
                        params![session_id],
                        |row| row.get::<_, i64>(0),
                    )
                    .optional()?;
                let segment_after = previous_cutoff.or(origin_seq);
                let round = match segment_after {
                    Some(after) if (current_generation.max(0) as u64) == generation => {
                        crate::live_compact::ordinary_round(&tx, session_id, after)?
                    }
                    _ => None,
                };
                let Some(round) = round else {
                    tx.execute("DELETE FROM pending_tasks WHERE id = ?1", params![id])?;
                    tx.execute(
                        "UPDATE sessions SET live_compact_requested = 0 WHERE id = ?1",
                        params![session_id],
                    )?;
                    continue;
                };
                compact_round = segment_after.map(|after| (round, after, generation));
            }

            let expanded = crate::meta::Expanded::from_json(&payload)
                .ok_or_else(|| anyhow::anyhow!("pending inject {id} has no message"))?;
            last = self.append_user_conn(&tx, session_id, &expanded)?;
            if let Some((round, segment_after, generation)) = compact_round {
                let request = crate::live_compact::Request {
                    request_seq: last,
                    segment_after_seq: segment_after,
                    round_start_seq: round.start_seq,
                    cutoff_seq: round.cutoff_seq,
                    generation,
                };
                Self::append_message_conn(
                    &tx,
                    session_id,
                    crate::live_compact::REQUEST_KIND,
                    &serde_json::to_value(request)?,
                )?;
            }
            tx.execute("DELETE FROM pending_tasks WHERE id = ?1", params![id])?;
            // The oldest wait is the one the operator felt.
            let age = now_millis() - written_at;
            if delivered == 0 || age > waited {
                waited = age;
            }
            delivered += 1;
        }
        tx.commit()?;
        Ok((delivered, waited, last))
    }

    /// Commits the terminal status and the fate of this turn's durable obligations together.
    ///
    /// # Invariant
    ///
    /// It also stamps `ended_at`, the column the session list is ordered by, so
    /// "the turn ended" and "the list may move" are the same commit: a reader
    /// never sees a session that finished while its place still says otherwise.
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
            "UPDATE sessions SET status = ?1, updated_at = ?2, ended_at = ?2 WHERE id = ?3",
            params![status_name(status), now_millis(), session_id],
        )?;
        if status != SessionStatus::Error {
            // Owed nothing here, and the next failure is a new episode.
            tx.execute(
                "UPDATE sessions SET monitor_seq = ?2, monitor_attempts = 0 WHERE id = ?1",
                params![session_id, now_millis()],
            )?;
            tx.execute(
                "UPDATE sessions
                    SET monitor_seq = COALESCE((SELECT MAX(m.seq) FROM messages m
                                                 WHERE m.session_id = sessions.id), 0)
                  WHERE id = ?1",
                params![session_id],
            )?;
        }
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
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    /// The messages one session has waiting for it, oldest first.
    ///
    /// # Invariant
    ///
    /// Only `inject` rows: a `turn` row is the turn's own obligation, written
    /// before the turn starts and deleted when it finishes, so counting it would
    /// report news on every call of a session that is simply working. A read, not
    /// a delivery -- what a step boundary does with these is `deliver_pending`'s
    /// call. See `docs/notes/server-store.md`.
    pub fn pending_injects_of(&self, session: &str) -> anyhow::Result<Vec<PendingTask>> {
        let conn = self.lock();
        let mut stmt = conn.prepare(
            "SELECT id, session, kind, payload, created_at FROM pending_tasks
              WHERE session = ?1 AND kind = 'inject' ORDER BY id",
        )?;
        let rows = stmt
            .query_map(params![session], |r| {
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
            "SELECT id, session, name, shell, job, pid, cmd, log, started_at, noticed
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
                    noticed: r.get::<_, i64>(9)? != 0,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    pub fn add_job(
        &self,
        session: &str,
        call_id: &str,
        tool: &str,
        description: Option<&str>,
        mode: &str,
        started_at: i64,
    ) -> anyhow::Result<i64> {
        let mut conn = self.lock();
        let tx = conn.transaction()?;
        tx.execute(
            "INSERT INTO tool_jobs (session, call_id, tool, description, mode, status, started_at)
             VALUES (?1, ?2, ?3, ?4, ?5, 'running', ?6)",
            params![session, call_id, tool, description, mode, started_at],
        )?;
        let id = tx.last_insert_rowid();
        tx.commit()?;
        Ok(id)
    }

    /// Writes how a job ended, and queues the notice that says so, in one act.
    ///
    /// # Invariant
    ///
    /// One row and at most one notice, committed together: a row left `running` is a
    /// job whose answer nobody has, which is exactly what the next start reports. The
    /// notice's session is the row's, so a job cannot speak for another session.
    pub fn finish_job(
        &self,
        id: i64,
        status: &str,
        output: &str,
        collected: bool,
        notice: Option<&serde_json::Value>,
    ) -> anyhow::Result<()> {
        let now = now_millis();
        let mut conn = self.lock();
        let tx = conn.transaction()?;
        tx.execute(
            "UPDATE tool_jobs SET status = ?2, output = ?3, collected = ?4, finished_at = ?5
              WHERE id = ?1",
            params![id, status, output, i64::from(collected), now],
        )?;
        if let Some(notice) = notice {
            tx.execute(
                "INSERT INTO pending_tasks (session, kind, payload, created_at)
                 SELECT session, 'inject', ?2, ?3 FROM tool_jobs WHERE id = ?1",
                params![id, serde_json::to_string(notice)?, now],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    /// Says which process is running a `detached` job, and where it will answer.
    ///
    /// # Invariant
    ///
    /// Written before the receipt leaves, so an id the caller can name is an id the next
    /// start can adopt: a row that never got its pid is a job nobody can find again.
    pub fn set_job_process(&self, id: i64, pid: u32, answer: &Path) -> anyhow::Result<()> {
        self.lock().execute(
            "UPDATE tool_jobs SET pid = ?2, answer = ?3 WHERE id = ?1",
            params![id, i64::from(pid), answer.display().to_string()],
        )?;
        Ok(())
    }

    /// One job, if this home has it.
    pub fn job(&self, id: i64) -> anyhow::Result<Option<JobRow>> {
        let conn = self.lock();
        let mut stmt = conn.prepare(
            "SELECT id, session, tool, description, status, output, collected,
                    started_at, finished_at, pid, answer
             FROM tool_jobs WHERE id = ?1",
        )?;
        let mut rows = stmt.query(params![id])?;
        match rows.next()? {
            Some(row) => Ok(Some(job_row(row)?)),
            None => Ok(None),
        }
    }

    /// Marks a job's answer as handed to its session, so it is not offered twice.
    pub fn collect_job(&self, id: i64) -> anyhow::Result<()> {
        self.lock().execute(
            "UPDATE tool_jobs SET collected = 1 WHERE id = ?1",
            params![id],
        )?;
        Ok(())
    }

    /// Every job a stopped run left either unanswered or unread, oldest first.
    pub fn owed_jobs(&self) -> anyhow::Result<Vec<JobRow>> {
        let conn = self.lock();
        let mut stmt = conn.prepare(
            "SELECT id, session, tool, description, status, output, collected,
                    started_at, finished_at, pid, answer
             FROM tool_jobs
             WHERE status = 'running' OR (status IN ('ok', 'error') AND collected = 0)
             ORDER BY id",
        )?;
        let rows = stmt
            .query_map([], job_row)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    /// Every job a session still has, running or not.
    pub fn jobs_of(&self, session: &str) -> anyhow::Result<Vec<i64>> {
        let conn = self.lock();
        let mut stmt = conn.prepare("SELECT id FROM tool_jobs WHERE session = ?1 ORDER BY id")?;
        let ids = stmt
            .query_map(params![session], |row| row.get(0))?
            .collect::<rusqlite::Result<Vec<i64>>>()?;
        Ok(ids)
    }

    /// Says a job will never answer, and keeps the row so `check_job` can say why.
    pub fn lose_job(&self, id: i64) -> anyhow::Result<()> {
        self.lock().execute(
            "UPDATE tool_jobs SET status = 'lost', finished_at = ?2
              WHERE id = ?1 AND status = 'running'",
            params![id, now_millis()],
        )?;
        Ok(())
    }

    /// Forgets one job: its session is gone, or nothing it said is left to read.
    pub fn delete_job(&self, id: i64) -> anyhow::Result<()> {
        self.lock()
            .execute("DELETE FROM tool_jobs WHERE id = ?1", params![id])?;
        Ok(())
    }

    /// Records that a session has been told this job ended.
    pub fn note_detached_noticed(&self, id: i64) -> anyhow::Result<()> {
        self.lock().execute(
            "UPDATE detached_jobs SET noticed = 1 WHERE id = ?1",
            params![id],
        )?;
        Ok(())
    }

    pub fn delete_detached_job(&self, id: i64) -> anyhow::Result<()> {
        self.lock()
            .execute("DELETE FROM detached_jobs WHERE id = ?1", params![id])?;
        Ok(())
    }

    // -- subscriptions ------------------------------------------------------

    /// Arms one subscription, and answers with the row that is now durable.
    ///
    /// # Invariant
    ///
    /// `created_at` is written and never read: it is when the session asked, which
    /// is what an operator needs to read a home's table by hand.
    pub fn add_subscription(
        &self,
        session: &str,
        name: &str,
        pattern: &str,
        persistent: bool,
        action: Option<crate::subscriptions::Action>,
    ) -> anyhow::Result<SubscriptionRow> {
        let (bash, cmd, python, code) = match action {
            Some(crate::subscriptions::Action::Bash { name, cmd }) => {
                (Some(name), Some(cmd), None, None)
            }
            Some(crate::subscriptions::Action::Python { name, code }) => {
                (None, None, Some(name), Some(code))
            }
            None => (None, None, None, None),
        };
        let conn = self.lock();
        // One name is one subscription: arming replaces the row it held.
        conn.execute(
            "DELETE FROM subscriptions WHERE session = ?1 AND name = ?2",
            params![session, name],
        )?;
        conn.execute(
            "INSERT INTO subscriptions
                 (session, name, pattern, persistent, bash, cmd, python, code, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                session,
                name,
                pattern,
                persistent,
                bash,
                cmd,
                python,
                code,
                now_millis()
            ],
        )?;
        Ok(SubscriptionRow {
            id: conn.last_insert_rowid(),
            session: session.to_string(),
            name: name.to_string(),
            pattern: pattern.to_string(),
            persistent,
            bash,
            cmd,
            python,
            code,
        })
    }

    /// Every armed subscription, oldest first, so a matcher can keep one list.
    pub fn subscriptions(&self) -> anyhow::Result<Vec<SubscriptionRow>> {
        let conn = self.lock();
        let mut stmt = conn.prepare(
            "SELECT id, session, name, pattern, persistent, bash, cmd, python, code
             FROM subscriptions ORDER BY id",
        )?;
        let rows = stmt
            .query_map([], |r| {
                Ok(SubscriptionRow {
                    id: r.get(0)?,
                    session: r.get(1)?,
                    name: r.get(2)?,
                    pattern: r.get(3)?,
                    persistent: r.get(4)?,
                    bash: r.get(5)?,
                    cmd: r.get(6)?,
                    python: r.get(7)?,
                    code: r.get(8)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    /// Drops one subscription, whether it fired, expired or was removed.
    pub fn delete_subscription(&self, id: i64) -> anyhow::Result<()> {
        self.lock()
            .execute("DELETE FROM subscriptions WHERE id = ?1", params![id])?;
        Ok(())
    }

    /// Drops the named subscription of one session; answers how many rows went.
    pub fn delete_subscription_name(&self, session: &str, name: &str) -> anyhow::Result<usize> {
        Ok(self.lock().execute(
            "DELETE FROM subscriptions WHERE session = ?1 AND name = ?2",
            params![session, name],
        )?)
    }

    /// Everything one session asked to be told about; a deleted session owes none.
    pub fn clear_subscriptions(&self, session: &str) -> anyhow::Result<usize> {
        Ok(self.lock().execute(
            "DELETE FROM subscriptions WHERE session = ?1",
            params![session],
        )?)
    }
}

/// What one session asked to be told about, as the database keeps it.
///
/// # Invariant
///
/// The row is the record and the matcher's list is a copy of it: a subscription
/// outlives the process that armed it, so a restart resumes from here rather than
/// from what a previous run happened to hold in memory.
#[derive(Debug, Clone)]
pub struct SubscriptionRow {
    pub id: i64,
    /// Who hears about a match.
    pub session: String,
    /// The name it is armed, re-armed and dropped by.
    pub name: String,
    /// The pattern, as it was typed.
    pub pattern: String,
    /// Whether it stays armed after it fires.
    pub persistent: bool,
    /// The session's named shell a match runs in, when it carries one.
    pub bash: Option<String>,
    /// The bash command run on a match.
    pub cmd: Option<String>,
    /// The session's named interpreter a match runs in, when it carries one.
    pub python: Option<String>,
    /// The Python code run on a match.
    pub code: Option<String>,
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
    /// Whether the session was already told this job ended.
    pub noticed: bool,
}

/// A tool call that said `detached` and has not reported yet.
///
/// # Invariant
///
/// The row is deleted in the same act that posts the call's report, so a row that
/// survives a start is a call whose server stopped before it could answer. See
/// `docs/notes/server-tools-detached.md`.
#[derive(Debug, Clone)]
pub struct JobRow {
    pub id: i64,
    pub session: String,
    pub tool: String,
    pub description: Option<String>,
    pub status: String,
    pub output: Option<String>,
    pub collected: bool,
    pub started_at: i64,
    pub finished_at: Option<i64>,
    /// The process running it, for a job that is not this server's task.
    pub pid: Option<i64>,
    /// The file that process writes its answer to.
    pub answer: Option<String>,
}

fn job_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<JobRow> {
    Ok(JobRow {
        id: row.get(0)?,
        session: row.get(1)?,
        tool: row.get(2)?,
        description: row.get(3)?,
        status: row.get(4)?,
        output: row.get(5)?,
        collected: row.get::<_, i64>(6)? != 0,
        started_at: row.get(7)?,
        finished_at: row.get(8)?,
        pid: row.get(9)?,
        answer: row.get(10)?,
    })
}

#[derive(Debug, Clone)]
pub struct SessionRow {
    pub id: i64,
    pub name: String,
    pub status: SessionStatus,
    /// The model this session runs on, `provider/model_name`; the bare name when
    /// the roster could not place it.
    pub model: String,
    /// The context level a notice is sent at, as a share of the window, when the
    /// session was given one.
    pub context_limit: Option<u8>,
    /// Reasoning level in the roster's spelling, when the session was given
    /// one. `None` means the model's own default.
    pub thinking_effort: Option<String>,
    pub cwd: String,
    pub created_at: i64,
    pub updated_at: i64,
    /// When its last turn ended; `None` is a session that never finished one.
    ///
    /// # Invariant
    ///
    /// Written by `finish_turn` and by nothing else, because the list is ordered
    /// by it: a tuning change has no turn to end.
    pub ended_at: Option<i64>,
    pub messages: u64,
    /// SPEC's `context (tok)`, `tps (latest)` and `ttft (latest)`: what the last
    /// completed request measured.
    pub usage: protocol::Usage,
    /// The endpoint this session's shells run on, when it is not `local`.
    pub remote: Option<String>,
    /// The tier every request sends, in the roster's spelling. `None` picks the
    /// shortest declared tier.
    pub cache_tier: Option<String>,
    /// Whether a periodic time notice keeps this session's prefix warm.
    pub cache_keepalive: bool,
}

/// What a clone wrote: the new session, its rows, and where it ends.
#[derive(Debug, Clone, Copy)]
pub struct CloneReport {
    pub id: i64,
    pub rows: usize,
    pub last_seq: i64,
}

/// One session that stopped with an error, as `Store::failures` reports it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Failure {
    pub id: i64,
    pub name: String,
    /// The model the failed turn ran on, `provider/model_name`.
    pub model: String,
    /// The last row a monitor turn was given, and how many models that turn was
    /// switched through.
    pub monitor_seq: i64,
    pub monitor_attempts: u32,
    /// The session's newest row: this is what a monitor turn has not seen yet.
    pub last_seq: i64,
}

/// One row of the event log, as stored: `payload` JSON, parsed by the caller.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EventRow {
    pub id: i64,
    pub seq: i64,
    pub session_id: Option<i64>,
    pub kind: String,
    pub payload: String,
    pub created_at: i64,
}

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
    /// A `tool_result`'s verdict, as the turn it belongs to recorded it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ok: Option<bool>,
    /// How long that tool ran, in milliseconds.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub elapsed_ms: Option<u64>,
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
            ok: None,
            elapsed_ms: None,
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
                // Absent on a row written before the outcome was kept: a read
                // of an old transcript then says nothing, not a guess.
                row.ok = value.get("ok").and_then(|v| v.as_bool());
                row.elapsed_ms = value.get("elapsed_ms").and_then(|v| v.as_u64());
            }
            // The harness talking to the reader: no role, no answer in the model's mouth.
            "notice" | "thinking_mark" => row.text = str_of("text").unwrap_or_default(),
            // The reader sees which prompt was in force, not 10k characters of it.
            "system" => {
                let cwd = str_of("cwd").unwrap_or_default();
                let chars = str_of("instructions").unwrap_or_default().chars().count();
                row.text = format!(
                    "[prompt in force] {chars} characters of instructions, read from {cwd}"
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
/// The stored row of a user message: its text, and the pictures it named.
fn user_row(expanded: &crate::meta::Expanded) -> serde_json::Value {
    let mut row = json!({"role": "user", "text": expanded.text});
    if !expanded.images.is_empty() {
        row["images"] = json!(
            expanded
                .images
                .iter()
                .map(|i| i.to_json())
                .collect::<Vec<_>>()
        );
    }
    row
}

fn wire_item(kind: &str, value: &serde_json::Value) -> Option<serde_json::Value> {
    let text_of = |key: &str| {
        value
            .get(key)
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string()
    };
    // Pictures stay in `display` and resolve per request.
    match kind {
        "text" => {
            let role = text_of("role");
            Some(if role == "assistant" {
                json!({"role": "assistant", "content": [{"type": "output_text", "text": text_of("text")}]})
            } else {
                json!({"role": role, "content": text_of("text")})
            })
        }
        "reasoning" => {
            // Byte for byte, never rebuilt; see `docs/notes/server-store.md`.
            if let Some(item) = value.get("item").filter(|item| item.is_object()) {
                return Some(item.clone());
            }
            let mut item = json!({"type": "reasoning", "summary": []});
            match value.get("encrypted").and_then(|v| v.as_str()) {
                Some(encrypted) => item["encrypted_content"] = json!(encrypted),
                // No blob -- an interrupted thought. The text is all there is, so it
                // is replayed as what it says rather than dropped.
                None => {
                    let text = value.get("summary").and_then(|v| v.as_str())?;
                    if text.trim().is_empty() {
                        return None;
                    }
                    item["summary"] = json!([{"type": "summary_text", "text": text}]);
                }
            }
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
        // A boundary the harness draws, not a thing the reader said.
        "thinking_mark" => Some(json!({"role": "user", "content": text_of("text")})),
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
            // Read back now, not at send time: replayability is the row's property.
            crate::llm::sendable_activity(item, replayable, crate::blobs::dir())
        }
        _ => None,
    }
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

/// The model's own name: what follows the provider in `provider/model_name`.
fn bare_model(model: &str) -> &str {
    model.split_once('/').map(|(_, name)| name).unwrap_or(model)
}

/// What a failed insert means.
///
/// # Invariant
///
/// Only a `UNIQUE` violation is "already exists". Every other constraint is a bug
/// in this module, and saying "already exists" for one would send a reader looking
/// for a session that is not there.
fn duplicate_session(name: &str, e: rusqlite::Error) -> anyhow::Error {
    let text = e.to_string();
    let unique = matches!(e, rusqlite::Error::SqliteFailure(ref f, _)
        if f.code == rusqlite::ErrorCode::ConstraintViolation)
        && text.contains("UNIQUE");
    if unique {
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
