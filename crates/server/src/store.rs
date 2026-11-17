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
    /// What every call may carry besides its own arguments, introduced once.
    ///
    /// # Invariant
    ///
    /// Stamped with the card, and `None` on a row written before this field existed:
    /// a session's context has two operations, append and cut, so a session already
    /// under way is sent as it was rather than told what it was never told.
    #[serde(default)]
    pub parameters: Option<String>,
    /// The spellings `index` carries, so a mention of one stays literal.
    #[serde(default)]
    pub rooted: Vec<String>,
    /// The directory the shell stood in, so a move is noticed.
    #[serde(default)]
    pub cwd: String,
    /// The model the card was stamped for, so a swap shows: a stale name lies.
    #[serde(default)]
    pub model: Option<String>,
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

/// The tool block a session sends, and the facts it was stamped from.
///
/// # Invariant
///
/// A stored row, like [`Prompt`]: what is sent is the context, and the context is
/// append-only, so a request reads this row instead of assembling a tool list. A
/// stamp is appended only when one of the facts below differs from the newest
/// row's, so the head moves at a stored change and nowhere else.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolSet {
    /// The block exactly as it goes on the wire, in this session's face.
    pub tools: serde_json::Value,
    /// The model it was stamped for: the roster's built-in tools follow the model.
    #[serde(default)]
    pub model: Option<String>,
    /// The peer it was stamped for: a move changes which machine a schema names.
    #[serde(default)]
    pub remote: Option<String>,
    /// The build that rendered it.
    ///
    /// # Invariant
    ///
    /// A build may rename or reshape a tool, so a session that outlived it must not
    /// keep advertising the old schema: the model would call a tool this server no
    /// longer has. Comparing it re-stamps once per build.
    #[serde(default)]
    pub build: String,
    /// Which stored change wrote this row. Read by a person, never compared.
    #[serde(default)]
    pub event: String,
}

/// What the model is told about a call that never returned: the
/// `interrupted_call` template.
pub fn interrupted_call() -> String {
    crate::prompts::harness_wrap(
        crate::prompts::shared()
            .text("interrupted_call")
            .unwrap_or_default(),
    )
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
    -- The branch and worktree this session was born in, by name. NULL is a session
    -- that made none, and it is fixed for the life of the row: the worktree comes
    -- and goes with the session that asked for it. See `docs/notes/server-worktree.md`.
    worktree TEXT,
    -- What that worktree holds, as the repository saw it when its turn last ended:
    -- NULL until something asked. `merged` is its branch reachable from the main line,
    -- `changed` is anything the main line does not have. See `docs/notes/server-worktree.md`.
    worktree_merged INTEGER,
    worktree_changed INTEGER,
    -- The last row of this session a monitor turn was given. A turn writes its
    -- user row before it starts, so `seq` only grows: this column is the whole
    -- record of "this failure has been reported", and it survives a restart.
    monitor_seq INTEGER NOT NULL DEFAULT 0,
    -- The tier every request of this session sends, in the roster's spelling.
    -- NULL picks the shortest tier the roster declares, so a row written before
    -- this column existed reads what a session that never asked reads.
    cache_tier TEXT,
    -- Whether a periodic time notice keeps this session's prefix warm. Off by
    -- default: a session that never asked starts no timer of its own.
    cache_keepalive INTEGER NOT NULL DEFAULT 0,
    -- What this session is for, in `explore`'s spelling. NULL is `work`, so a row
    -- written before this column existed reads what a session that never asked
    -- reads. See `docs/notes/server-explore.md`.
    mode TEXT,
    -- Whether the server asks this session to fold its own context, and how far its
    -- context may grow between checkpoints. NULL follows the home's `[agent]`
    -- setting: the switch is a session's own, the default is the file's, and a
    -- session that never asked reads the file.
    live_compact INTEGER,
    live_compact_interval_tokens INTEGER,
    -- When this session's last turn ended, and nothing else. NULL is a session
    -- that has not finished a turn yet, which the list orders by `created_at`.
    -- Its own column rather than `updated_at`, which a tuning change rewrites:
    -- the list is ordered by this, so writing it anywhere else would move a row
    -- for a reason the operator did not give.
    ended_at INTEGER,
    -- SPEC's canceled flag: `cancel` cut this session's turn, and the next start
    -- continues it. Raised by `cancel` on a session that was running and not
    -- inside `wait`, cleared by the start that claims the turn. Its own column
    -- because a flag in the process is a flag a stop cannot leave behind, and a
    -- pending row is not the same fact: a turn can owe work with no row to say so.
    canceled INTEGER NOT NULL DEFAULT 0,
    -- SPEC's waiting flag, with the time it was waiting for: a stop that finds a
    -- session inside `wait` does not continue it at once, it continues that wait
    -- for the time that was left. Epoch milliseconds, NULL for a session that is
    -- not waiting. The same reason `canceled` is a column: a stop ends the
    -- process, and a deadline only it knew would be a wait nobody resumes.
    waiting_until INTEGER,
    -- SPEC's Dreaming: the session this row is a disposable dream of, by id.
    -- NULL is an ordinary session. No foreign key: a dream goes through the
    -- delete machinery, which ends its channels and subscriptions too, and a
    -- source's delete discards its dreams the same way.
    -- See `docs/notes/server-dream.md`.
    dream_of INTEGER,
    -- The checkpoint generation this row's dreaming concerns: on a source, the
    -- generation its last dream was spawned for; on a dream, the generation it
    -- was spawned for. NULL is a row that has never dreamed.
    dreamed_checkpoint_id INTEGER
);
CREATE INDEX IF NOT EXISTS sessions_by_status ON sessions(status);

-- A session's rows can still be deleted -- by the agent, from a shell, through the
-- `handoff` skill, and only after the whole transcript has been copied into a mentor
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
    -- Which turn and step wrote this row, so a step is a range and not a walk.
    -- NULL for a row that stands in no step: the prompt in force, a notice, and
    -- the user's line, which opens a turn before the turn has a number.
    turn       INTEGER,
    step       INTEGER,
    created_at INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS messages_by_session ON messages(session_id, seq);
-- A call and its answer are paired by their number, and a start asks that of every
-- call in every session: without this the question is a scan of the session per
-- call, which is minutes of a large home's start. See `docs/notes/server-store.md`.
CREATE INDEX IF NOT EXISTS messages_by_call ON messages(session_id, call_id, kind);

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
    -- The provider's own summary of the thought, when it streamed one: a different
    -- field of the same item, never merged into `text`.
    summary    TEXT NOT NULL DEFAULT '',
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
-- session and a `cancel`, so this table is how the next run finds what a cancel
-- let go: a server that ends any other way takes its jobs with it, and the rows
-- it left name processes that are gone.
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

-- A tool call that did not wait for its answer used to be a row here. The two 
-- arguments that made one (`background`, `timeout_ms`) are gone: a call that does
-- not wait is a channel, read later. A table an older build wrote is dropped.
DROP TABLE IF EXISTS tool_jobs;

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
    action     TEXT,
    created_at INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS subscriptions_by_session ON subscriptions(session);

-- The pictures a message named, copied into this home: the row names the copy by
-- id, and the bytes live beside the database. See `docs/notes/server-pictures.md`.
CREATE TABLE IF NOT EXISTS channels (
    id           INTEGER PRIMARY KEY,
    session      TEXT NOT NULL,
    path         TEXT NOT NULL,
    program      TEXT NOT NULL,
    remote       TEXT,
    dir          TEXT NOT NULL,
    pid          INTEGER NOT NULL,
    started_at   INTEGER NOT NULL,
    upto         INTEGER NOT NULL DEFAULT 0,
    -- When this channel was last written to: what a call's `time` is measured
    -- from. NULL until the first write, so the program's start stands then.
    wrote_at     INTEGER,
    -- What a caller is waiting for, and whether one is: the marker a call has to
    -- see (`''` for the end alone), or NULL for nobody waiting.
    wait_marker  TEXT,
    -- The marker this channel's last write ended its reply with: SPEC's "sticky
    -- value", so a read that writes nothing still knows where a reply ends.
    until_sentinel TEXT NOT NULL DEFAULT '',
    -- Whether a `cancel` was asked to keep this channel alive. A state, not a
    -- moment: the flag is written by `cancel(keep_alive)`, read by whoever comes
    -- next, and cleared only when the relay is adopted or the channel ends.
    -- Without it a later stop that did not ask -- a window leaving, a signal --
    -- ends what an earlier one kept, and `keep_alive` would mean only "until the
    -- next stop". See `docs/notes/server-channel.md`.
    detached INTEGER NOT NULL DEFAULT 0,
    -- The relay's own pid, which is not the program's: `pid` is what the program
    -- runs as, read from the relay's `status`. A successor that only has `pid`
    -- can see the program is gone but cannot find the relay to adopt, so a kept
    -- channel would be unadoptable. `NULL` is a row written before this column.
    relay_pid INTEGER,
    -- When the relay started, to prove a pid is still the same process. A pid is
    -- only unique until it is recycled, and adoption arms a job that kills on
    -- close, so a recycled pid would make this server's death somebody else's.
    relay_started_at INTEGER,
    UNIQUE (session, path)
);

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
fn migrate(conn: &Connection) -> anyhow::Result<()> {
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
    // `cancel`'s flag, under the name of the verb that owns it.
    if has_column(conn, "sessions", "interrupted") && !has_column(conn, "sessions", "canceled") {
        conn.execute(
            "ALTER TABLE sessions RENAME COLUMN interrupted TO canceled",
            [],
        )?;
    }
    if has_column(conn, "sessions", "model") && !has_column(conn, "sessions", "model_name") {
        conn.execute("ALTER TABLE sessions RENAME COLUMN model TO model_name", [])?;
    }
    // No reader left: a monitor is an ordinary session, so nothing counts the
    // models one was switched through.
    if has_column(conn, "sessions", "monitor_attempts") {
        conn.execute("ALTER TABLE sessions DROP COLUMN monitor_attempts", [])?;
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
        ("worktree", "ALTER TABLE sessions ADD COLUMN worktree TEXT"),
        (
            "worktree_merged",
            "ALTER TABLE sessions ADD COLUMN worktree_merged INTEGER",
        ),
        (
            "worktree_changed",
            "ALTER TABLE sessions ADD COLUMN worktree_changed INTEGER",
        ),
        ("ttft_ms", "ALTER TABLE sessions ADD COLUMN ttft_ms INTEGER"),
        (
            "monitor_seq",
            "ALTER TABLE sessions ADD COLUMN monitor_seq INTEGER NOT NULL DEFAULT 0",
        ),
        (
            "cache_tier",
            "ALTER TABLE sessions ADD COLUMN cache_tier TEXT",
        ),
        (
            "cache_keepalive",
            "ALTER TABLE sessions ADD COLUMN cache_keepalive INTEGER NOT NULL DEFAULT 0",
        ),
        ("mode", "ALTER TABLE sessions ADD COLUMN mode TEXT"),
        (
            "live_compact",
            "ALTER TABLE sessions ADD COLUMN live_compact INTEGER",
        ),
        (
            "live_compact_interval_tokens",
            "ALTER TABLE sessions ADD COLUMN live_compact_interval_tokens INTEGER",
        ),
        (
            "dream_of",
            "ALTER TABLE sessions ADD COLUMN dream_of INTEGER",
        ),
        (
            "dreamed_checkpoint_id",
            "ALTER TABLE sessions ADD COLUMN dreamed_checkpoint_id INTEGER",
        ),
    ] {
        add_column_if_missing(conn, "sessions", column, ddl)?;
    }
    // A channel says when a caller waits on it, and for what.
    add_column_if_missing(
        conn,
        "channels",
        "wait_marker",
        "ALTER TABLE channels ADD COLUMN wait_marker TEXT",
    )?;
    // A channel says when a caller waits on it, and for what.
    add_column_if_missing(
        conn,
        "channels",
        "wait_marker",
        "ALTER TABLE channels ADD COLUMN wait_marker TEXT",
    )?;
    // A channel a `cancel` was asked to keep alive: a state on the row, so the
    // next stop can tell it from a channel nobody asked for.
    add_column_if_missing(
        conn,
        "channels",
        "detached",
        "ALTER TABLE channels ADD COLUMN detached INTEGER NOT NULL DEFAULT 0",
    )?;
    // The relay's own pid and start: `pid` alone cannot find a relay to adopt.
    add_column_if_missing(
        conn,
        "channels",
        "relay_pid",
        "ALTER TABLE channels ADD COLUMN relay_pid INTEGER",
    )?;
    add_column_if_missing(
        conn,
        "channels",
        "relay_started_at",
        "ALTER TABLE channels ADD COLUMN relay_started_at INTEGER",
    )?;
    add_column_if_missing(
        conn,
        "sessions",
        "ended_at",
        "ALTER TABLE sessions ADD COLUMN ended_at INTEGER",
    )?;
    add_column_if_missing(
        conn,
        "sessions",
        "canceled",
        "ALTER TABLE sessions ADD COLUMN canceled INTEGER NOT NULL DEFAULT 0",
    )?;
    add_column_if_missing(
        conn,
        "sessions",
        "waiting_until",
        "ALTER TABLE sessions ADD COLUMN waiting_until INTEGER",
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
    // A draft keeps the two texts apart, so a cut stream can still be sent back.
    add_column_if_missing(
        conn,
        "drafts",
        "summary",
        "ALTER TABLE drafts ADD COLUMN summary TEXT NOT NULL DEFAULT ''",
    )?;
    conn.execute(
        "UPDATE detached_jobs SET name = shell || '-' || job WHERE name = ''",
        [],
    )?;
    // The table `detached` calls were copied into is gone: drop the source too.
    if has_column(conn, "detached_calls", "call_id") {
        conn.execute_batch("DROP TABLE detached_calls;")?;
    }
    add_column_if_missing(
        conn,
        "messages",
        "call_id",
        "ALTER TABLE messages ADD COLUMN call_id TEXT",
    )?;
    // A row's step was only in its payload; as columns it is a range read.
    if !has_column(conn, "messages", "step") {
        let answered = seal_calls(conn)?;
        if answered > 0 {
            tracing::info!(answered, "answered the calls a transcript ended inside");
        }
        conn.execute("ALTER TABLE messages ADD COLUMN turn INTEGER", [])?;
        conn.execute("ALTER TABLE messages ADD COLUMN step INTEGER", [])?;
        backfill_steps(conn)?;
    }
    conn.execute_batch(
        "CREATE INDEX IF NOT EXISTS messages_by_step ON messages(session_id, turn, step)",
    )?;
    add_column_if_missing(
        conn,
        "channels",
        "wrote_at",
        "ALTER TABLE channels ADD COLUMN wrote_at INTEGER",
    )?;
    add_column_if_missing(
        conn,
        "channels",
        "until_sentinel",
        "ALTER TABLE channels ADD COLUMN until_sentinel TEXT NOT NULL DEFAULT ''",
    )?;
    // An older build's ended rows held names for nothing: swept, then their columns go.
    if has_column(conn, "channels", "ended_at") {
        let corpses = conn.execute("DELETE FROM channels WHERE ended_at IS NOT NULL", [])?;
        if corpses > 0 {
            tracing::info!(
                corpses,
                "released the names of channels whose program had ended"
            );
        }
        conn.execute("ALTER TABLE channels DROP COLUMN ended_at", [])?;
    }
    if has_column(conn, "channels", "exit") {
        conn.execute("ALTER TABLE channels DROP COLUMN exit", [])?;
    }
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
        ("action", "ALTER TABLE subscriptions ADD COLUMN action TEXT"),
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
         END;

         CREATE TRIGGER IF NOT EXISTS context_no_update
         BEFORE UPDATE ON messages
         BEGIN
             SELECT RAISE(ABORT, 'context is append only: no API may change a written row');
         END;

         CREATE TRIGGER IF NOT EXISTS context_cut_tail_only
         BEFORE DELETE ON messages
         WHEN OLD.seq < (SELECT MAX(seq) FROM messages WHERE session_id = OLD.session_id)
              AND EXISTS(SELECT 1 FROM sessions WHERE id = OLD.session_id)
         BEGIN
             SELECT RAISE(ABORT, 'context may only be cut at its tail');
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
                     'inject', ?2, ?3, NULL, ?4)",
            params![
                session_id,
                wire_item(Kind::Inject, &value).map(|item| item.to_string()),
                value.to_string(),
                now_millis()
            ],
        )?;
    }
    tx.execute("ALTER TABLE sessions DROP COLUMN handover", [])?;
    tx.commit()
}

/// The row that answers a call nothing answered, in the step the call is in.
fn interrupted_result(call_id: &str, call: &serde_json::Value) -> serde_json::Value {
    let mut payload = json!({
        "call_id": call_id,
        "output": interrupted_call(),
        "images": [],
        "step": call.get("step").cloned().unwrap_or_default(),
        "ok": false,
        "elapsed_ms": 0,
        "tool": call.get("name").and_then(|v| v.as_str()).unwrap_or_default(),
    });
    // The answer belongs to the step the call is in, not to one of its own.
    if let Some(turn) = call.get("turn") {
        payload["turn"] = turn.clone();
    }
    payload
}

/// Answers the calls a transcript ended inside, before a row stated its own step.
///
/// # Invariant
///
/// Runs once, on the open that adds `turn` and `step`, and *before* it: from then
/// on the question is a row's own columns, so an older call has to be answered while
/// the transcript still answers for it. A crash leaves the columns absent and the
/// next start asks again; answering twice does nothing, since the result it writes
/// is what the next question sees.
fn seal_calls(conn: &Connection) -> anyhow::Result<usize> {
    let rows: Vec<(i64, String, String)> = {
        let mut stmt = conn.prepare(
            "SELECT c.session_id, c.call_id, c.display FROM messages c
              WHERE c.kind = 'tool_call' AND c.call_id IS NOT NULL
                AND NOT EXISTS (
                    SELECT 1 FROM messages r
                     WHERE r.session_id = c.session_id AND r.kind = 'tool_result'
                       AND r.call_id = c.call_id)
              ORDER BY c.session_id, c.seq",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?, row.get(2)?))
        })?;
        rows.collect::<rusqlite::Result<_>>()?
    };
    let mut answered = 0;
    for (session_id, call_id, display) in rows {
        let call: serde_json::Value = serde_json::from_str(&display).unwrap_or_default();
        let payload = interrupted_result(&call_id, &call);
        // Its own insert: `append_message` writes the columns this makes room for.
        conn.query_row(
            "INSERT INTO messages (session_id, seq, kind, content, display, call_id, created_at)
             VALUES (
                ?1,
                (SELECT COALESCE(MAX(seq), 0) + 1 FROM messages WHERE session_id = ?1),
                ?2, ?3, ?4, ?5, ?6
             )
             RETURNING seq",
            params![
                session_id,
                Kind::ToolResult.as_str(),
                wire_item(Kind::ToolResult, &payload).map(|item| item.to_string()),
                serde_json::to_string(&payload)?,
                call_id,
                now_millis()
            ],
            |r| r.get::<_, i64>(0),
        )?;
        conn.execute(
            "UPDATE tool_calls SET output = ?1, status = 'cancelled', finished_at = ?2
              WHERE session_id = ?3 AND call_id = ?4",
            params![interrupted_call(), now_millis(), session_id, call_id],
        )?;
        answered += 1;
    }
    Ok(answered)
}

/// Gives the rows written before turn and step were columns the ones their payload
/// already names.
///
/// # Invariant
///
/// Reads the payload, never guesses: a row that names neither stays NULL, which is
/// how "belongs to no step" is spelled. A payload that is not JSON is skipped
/// rather than failing the migration of a whole home.
fn backfill_steps(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute(
        "UPDATE messages
            SET turn = json_extract(display, '$.turn'),
                step = json_extract(display, '$.step')
          WHERE json_valid(display)
            AND (json_extract(display, '$.turn') IS NOT NULL
                 OR json_extract(display, '$.step') IS NOT NULL)",
        [],
    )?;
    Ok(())
}

/// Gives the rows a version-1 home wrote the item their requests will send.
///
/// # Invariant
///
/// The split itself is the version step, which renames the column before the database
/// opens; this applies the writer's own rule to the rows that predate it, so what a row
/// sends has one spelling. A row with no `content` and a wire form of its own was never
/// split, which makes this a no-op on a home that has been through it.
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
        // No wire form is no split: a `system` or `tools` row keeps `content` empty.
        let Some(content) = Kind::parse(&kind).and_then(|kind| wire_item(kind, &value)) else {
            continue;
        };
        conn.execute(
            "UPDATE messages SET content = ?1, call_id = ?2 WHERE id = ?3",
            params![
                content.to_string(),
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
        })
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

    /// Creates a session on a model, on a machine, in the worktree it was born in.
    ///
    /// # Invariant
    ///
    /// Both model columns are written, and `model_name` is the same model without
    /// its provider. A renamed column keeps its `NOT NULL`, so a row that named
    /// only one of them could not be inserted at all -- and a bare name is right
    /// for a row the roster cannot place.
    ///
    /// The worktree is written with the directory it was made at, in one statement.
    pub fn create_session_on(
        &self,
        name: &str,
        model: &str,
        cwd: &str,
        remote: Option<&str>,
        worktree: Option<&str>,
    ) -> anyhow::Result<i64> {
        if name.trim().is_empty() {
            anyhow::bail!("session name must not be empty");
        }
        let now = now_millis();
        let mut conn = self.lock();
        let tx = conn.transaction()?;
        tx.execute(
            "INSERT INTO sessions (name, status, model, model_name, cwd, created_at, updated_at,
                                   remote, worktree, worktree_merged, worktree_changed)
             VALUES (?1, 'idle', ?2, ?3, ?4, ?5, ?5, ?6, ?7, ?8, ?9)",
            params![
                name,
                model,
                bare_model(model),
                cwd,
                now,
                remote,
                worktree,
                // A worktree is made at the main line's own commit and touches nothing,
                // so this is read, not guessed.
                worktree.map(|_| 1i64),
                worktree.map(|_| 0i64)
            ],
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
    ///
    /// It owns no worktree even when the source does: a branch has one session.
    /// A `dream` copy: see `Dream`.
    pub fn clone_session(
        &self,
        source: &str,
        name: &str,
        dream: Option<Dream>,
    ) -> anyhow::Result<CloneReport> {
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
                                       tps, ttft_ms, remote, monitor_seq,
                                       cache_tier, cache_keepalive, mode, live_compact,
                                       dream_of, dreamed_checkpoint_id)
                 SELECT ?2, 'idle', model, model_name, cwd, ?3, ?3, context_limit,
                        thinking_effort, context_tokens, context_generation, cached_tokens,
                        tps, ttft_ms, remote, monitor_seq, cache_tier,
                        CASE WHEN ?4 IS NULL THEN cache_keepalive ELSE 0 END,
                        CASE WHEN ?4 IS NULL THEN mode ELSE 'work' END,
                        CASE WHEN ?4 IS NULL THEN NULL ELSE 0 END,
                        ?4, ?5
                   FROM sessions WHERE name = ?1",
                params![
                    source,
                    name,
                    now,
                    dream.map(|d| d.source),
                    dream.map(|d| d.generation)
                ],
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

    /// The latest checkpoint of a session: the generation a dream is named for.
    /// `None` is generation 0.
    pub fn latest_checkpoint_id(&self, session_id: i64) -> anyhow::Result<Option<i64>> {
        let conn = self.lock();
        let id = conn
            .query_row(
                "SELECT id FROM live_compact_checkpoints
                  WHERE session_id = ?1 ORDER BY id DESC LIMIT 1",
                params![session_id],
                |r| r.get(0),
            )
            .optional()?;
        Ok(id)
    }

    /// Every live dream of a source, oldest first: at most one is allowed.
    pub fn dreams_of(&self, source: i64) -> anyhow::Result<Vec<(i64, String)>> {
        let conn = self.lock();
        let mut stmt =
            conn.prepare("SELECT id, name FROM sessions WHERE dream_of = ?1 ORDER BY id")?;
        let rows = stmt
            .query_map(params![source], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// Marks the generation a source last dreamed, so the next fire of the same
    /// generation sends the time notice instead.
    pub fn set_dreamed_checkpoint(&self, id: i64, generation: i64) -> anyhow::Result<()> {
        self.lock().execute(
            "UPDATE sessions SET dreamed_checkpoint_id = ?2 WHERE id = ?1",
            params![id, generation],
        )?;
        Ok(())
    }

    /// Lets a generation dream again, but only while the source still marks that
    /// generation.
    pub fn reset_dreamed_checkpoint(&self, id: i64, generation: i64) -> anyhow::Result<()> {
        self.lock().execute(
            "UPDATE sessions SET dreamed_checkpoint_id = NULL
              WHERE id = ?1 AND dreamed_checkpoint_id = ?2",
            params![id, generation],
        )?;
        Ok(())
    }

    /// Dreams whose true-end discard was lost to a process exit: no owed row, no stop flag.
    pub fn orphan_dreams(&self) -> anyhow::Result<Vec<(i64, String, i64, Option<i64>)>> {
        let conn = self.lock();
        let mut stmt = conn.prepare(
            "SELECT s.id, s.name, s.dream_of, s.dreamed_checkpoint_id FROM sessions s
              WHERE s.dream_of IS NOT NULL
                AND s.canceled = 0 AND s.waiting_until IS NULL
                AND NOT EXISTS (SELECT 1 FROM pending_tasks p
                                 WHERE p.session = s.name AND p.kind IN ('turn', 'inject'))",
        )?;
        let rows = stmt
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
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
        // A draft left behind is settled at the next start into a row of a dead session.
        tx.execute("DELETE FROM drafts WHERE session_id = ?1", params![id])?;
        // The work it was owed, the jobs it started, and the answers it never read.
        for table in ["pending_tasks", "detached_jobs"] {
            tx.execute(
                &format!("DELETE FROM {table} WHERE session = ?1"),
                params![name],
            )?;
        }
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
    /// still names -- a `handoff` copy, a second session that asked for the same item --
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
                    s.cache_tier, s.cache_keepalive, s.ended_at, s.mode,
                    s.live_compact, s.live_compact_interval_tokens, s.worktree,
                    s.worktree_merged, s.worktree_changed,
                    s.dream_of, s.dreamed_checkpoint_id
             FROM sessions s";

    /// Every session in `error` because of a provider, with what a monitor turn
    /// still owes it.
    ///
    /// # Invariant
    ///
    /// The status is the trigger and the column is the memory, so a failure is
    /// reported once and once only, across restarts. See
    /// `docs/notes/server-monitor.md`.
    ///
    /// The origin is the gate, read from the failed turn's end row: a session that
    /// broke elsewhere stays in `error` and is nobody's report. A row predating
    /// the origin counts as "not the provider's".
    pub fn failures(&self) -> anyhow::Result<Vec<Failure>> {
        let conn = self.lock();
        let mut stmt = conn.prepare(
            "SELECT s.id, s.name, COALESCE(s.model, s.model_name),
                    s.monitor_seq,
                    COALESCE((SELECT MAX(m.seq) FROM messages m WHERE m.session_id = s.id), 0)
             FROM sessions s WHERE s.status = 'error'
               AND COALESCE((SELECT json_extract(e.payload, '$.origin') FROM events e
                             WHERE e.session_id = s.id AND e.kind = 'turn_end'
                             ORDER BY e.id DESC LIMIT 1), '') = 'provider'
             ORDER BY s.id",
        )?;
        let rows = stmt
            .query_map([], |r| {
                Ok(Failure {
                    id: r.get(0)?,
                    name: r.get(1)?,
                    model: r.get(2)?,
                    monitor_seq: r.get(3)?,
                    last_seq: r.get(4)?,
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
    pub fn mark_monitored(&self, session_id: i64, seq: i64) -> anyhow::Result<()> {
        let conn = self.lock();
        conn.execute(
            "UPDATE sessions SET monitor_seq = ?2 WHERE id = ?1",
            params![session_id, seq],
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
            mode: crate::explore::Mode::of(r.get::<_, Option<String>>(18)?.as_deref()),
            live_compact_enabled: r.get::<_, Option<i64>>(19)?.map(|stated| stated != 0),
            live_compact_interval_tokens: r
                .get::<_, Option<i64>>(20)?
                .map(|tokens| tokens.max(0) as u64),
            worktree: r.get(21)?,
            worktree_merged: r.get::<_, Option<i64>>(22)?.map(|v| v != 0),
            worktree_changed: r.get::<_, Option<i64>>(23)?.map(|v| v != 0),
            dream_of: r.get(24)?,
            dreamed_checkpoint_id: r.get(25)?,
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

    /// A session's own live compact: the switch, the threshold, and the request
    /// they stop owing.
    ///
    /// # Invariant
    ///
    /// Switching off drops a request already queued. The notice is the server
    /// asking, so a session that is no longer asked owes no summary, and a
    /// checkpoint written for a request nobody wants would fold a segment the
    /// operator just said to keep.
    pub fn set_session_live_compact(
        &self,
        id: i64,
        session: &str,
        enabled: Option<bool>,
        interval_tokens: Option<u64>,
    ) -> anyhow::Result<()> {
        let mut conn = self.lock();
        let tx = conn.transaction()?;
        tx.execute(
            "UPDATE sessions SET live_compact = ?1, live_compact_interval_tokens = ?2,
                    updated_at = ?3
              WHERE id = ?4",
            params![
                enabled.map(i64::from),
                interval_tokens.map(|tokens| tokens as i64),
                now_millis(),
                id
            ],
        )?;
        // Off, or a new base: the backoff measured a stretch this session left.
        if enabled == Some(false) || interval_tokens.is_some() {
            tx.execute(
                "UPDATE sessions SET live_compact_interval_effective = NULL WHERE id = ?1",
                params![id],
            )?;
        }
        if enabled == Some(false) {
            tx.execute(
                "DELETE FROM pending_tasks
                  WHERE session = ?1 AND kind = 'inject'
                    AND json_extract(payload, '$.live_compact') IS NOT NULL",
                params![session],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    /// What a session's worktree holds, as its repository answered.
    pub fn set_worktree_state(&self, id: i64, merged: bool, changed: bool) -> anyhow::Result<()> {
        self.lock().execute(
            "UPDATE sessions SET worktree_merged = ?1, worktree_changed = ?2 WHERE id = ?3",
            params![i64::from(merged), i64::from(changed), id],
        )?;
        Ok(())
    }

    /// What this session is for, in `explore`'s spelling, as a named command set it.
    pub fn set_session_mode(&self, id: i64, mode: &str) -> anyhow::Result<()> {
        self.lock().execute(
            "UPDATE sessions SET mode = ?1, updated_at = ?2 WHERE id = ?3",
            params![mode, now_millis(), id],
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

    /// Records what one stop leaves a session: canceled, or a wait with its clock.
    ///
    /// # Invariant
    ///
    /// The two are exclusive. SPEC's `cancel` continues a session that was running
    /// and not waiting at the next start, and a session inside `wait` by its own
    /// clock instead; neither flag is a session that was idle. One write, so a row
    /// can never say both, and a start that picks either up clears both.
    pub fn record_stop(
        &self,
        id: i64,
        canceled: bool,
        waiting_until: Option<i64>,
    ) -> anyhow::Result<()> {
        self.lock().execute(
            "UPDATE sessions SET canceled = ?1, waiting_until = ?2 WHERE id = ?3",
            params![i64::from(canceled), waiting_until, id],
        )?;
        Ok(())
    }

    /// Every session a stop left owing a turn, oldest first.
    ///
    /// # Invariant
    ///
    /// Read once, by the start that inherits them: a canceled session continues
    /// even when no pending row survived, which is what makes the flag the memory
    /// rather than a decoration on the rows.
    pub fn canceled_sessions(&self) -> anyhow::Result<Vec<(i64, String)>> {
        let conn = self.lock();
        let mut stmt =
            conn.prepare("SELECT id, name FROM sessions WHERE canceled <> 0 ORDER BY id")?;
        let rows = stmt
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// Every session a stop left inside `wait`, with the instant that wait ends.
    ///
    /// # Invariant
    ///
    /// Read once, by the start that inherits them, and consumed there: a wait is a
    /// fact about one turn, so a row that keeps it would resume a turn that ended.
    pub fn waiting_sessions(&self) -> anyhow::Result<Vec<(i64, String, i64)>> {
        let conn = self.lock();
        let mut stmt = conn.prepare(
            "SELECT id, name, waiting_until FROM sessions \
             WHERE waiting_until IS NOT NULL ORDER BY waiting_until, id",
        )?;
        let rows = stmt
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    // -- transcript --------------------------------------------------------

    /// Appends a message and returns its sequence number within the session.
    pub fn append_message(
        &self,
        session_id: i64,
        kind: Kind,
        content: &serde_json::Value,
    ) -> anyhow::Result<i64> {
        Self::append_message_conn(&self.lock(), session_id, kind, content)
    }

    fn append_message_conn(
        conn: &rusqlite::Connection,
        session_id: i64,
        kind: Kind,
        content: &serde_json::Value,
    ) -> anyhow::Result<i64> {
        let seq: i64 = conn.query_row(
            "INSERT INTO messages
                 (session_id, seq, kind, content, display, call_id, turn, step, created_at)
             VALUES (
                ?1,
                (SELECT COALESCE(MAX(seq), 0) + 1 FROM messages WHERE session_id = ?1),
                ?2,
                ?3,
                ?4,
                ?5,
                ?6,
                ?7,
                ?8
             )
             RETURNING seq",
            params![
                session_id,
                kind.as_str(),
                wire_item(kind, content).map(|item| item.to_string()),
                serde_json::to_string(content)?,
                content.get("call_id").and_then(|v| v.as_str()),
                content
                    .get("turn")
                    .and_then(|v| v.as_u64())
                    .map(|v| v as i64),
                content
                    .get("step")
                    .and_then(|v| v.as_u64())
                    .map(|v| v as i64),
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
    /// The input tokens the last measurement counted for this session; 0 when none yet.
    pub fn measured_input_tokens(&self, session_id: i64) -> anyhow::Result<u64> {
        let tokens: i64 = self.lock().query_row(
            "SELECT context_tokens FROM sessions WHERE id = ?1",
            params![session_id],
            |row| row.get(0),
        )?;
        Ok(tokens.max(0) as u64)
    }

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
        // One row at a time from the tail: the append-only trigger aborts a bulk delete.
        let mut count = 0usize;
        loop {
            let removed = tx.execute(
                "DELETE FROM messages
                  WHERE session_id = ?1
                    AND seq = (SELECT MAX(seq) FROM messages WHERE session_id = ?1)
                    AND seq > ?2",
                params![session_id, seq],
            )?;
            if removed == 0 {
                break;
            }
            count += removed;
        }
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
                // The four measured columns go together: the transcript this request
                // was billed for is gone, so its rate and its delay describe nothing
                // left in this row. See `docs/notes/server-store.md`.
                "UPDATE sessions SET live_compact_baseline_tokens = NULL,
                        live_compact_interval_effective = NULL,
                        context_tokens = 0, cached_tokens = NULL,
                        tps = NULL, ttft_ms = NULL,
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
        self.append_message(session_id, Kind::System, &payload)
    }

    /// The newest stored tool set, or nothing before a session's first stamp.
    pub fn tool_set(&self, session_id: i64) -> anyhow::Result<Option<ToolSet>> {
        let row = {
            let conn = self.lock();
            conn.query_row(
                "SELECT display FROM messages \
                  WHERE session_id = ?1 AND kind = 'tools' ORDER BY seq ASC LIMIT 1",
                params![session_id],
                |r| r.get::<_, String>(0),
            )
            .optional()?
        };
        let Some(display) = row else { return Ok(None) };
        Ok(serde_json::from_str::<ToolSet>(&display).ok())
    }

    /// Writes the head once. Context has two operations, append and cut, and a schema
    /// is neither.
    ///
    /// # Invariant
    ///
    /// Refused here rather than trusted to callers. A head already written makes
    /// this an error, so no code path -- a setting, a skill, a tool, a command --
    /// can express appending another schema. Settings are not context.
    pub fn write_head_once(&self, session_id: i64, tool_set: &ToolSet) -> anyhow::Result<()> {
        let already = {
            let conn = self.lock();
            conn.query_row(
                "SELECT EXISTS(SELECT 1 FROM messages WHERE session_id = ?1 AND kind = 'tools')",
                params![session_id],
                |r| r.get::<_, bool>(0),
            )?
        };
        if already {
            anyhow::bail!(
                "this session already has a head; a schema is not appendable (context is append and cut only)"
            );
        }
        let payload = serde_json::to_value(tool_set)?;
        self.append_message(session_id, Kind::Tools, &payload)?;
        Ok(())
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
        summary: &str,
    ) -> anyhow::Result<()> {
        let conn = self.lock();
        conn.execute(
            "INSERT INTO drafts (session_id, item_id, turn, part, text, summary, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
             ON CONFLICT(session_id, item_id) DO UPDATE SET
                 text = excluded.text,
                 summary = excluded.summary,
                 part = excluded.part,
                 turn = excluded.turn,
                 updated_at = excluded.updated_at",
            params![
                session_id,
                item_id,
                turn as i64,
                part,
                text,
                summary,
                now_millis()
            ],
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
        let pending: Vec<(String, u64, String, String, String)> = {
            let conn = self.lock();
            let mut stmt = conn.prepare(
                "SELECT item_id, turn, part, text, summary FROM drafts
                  WHERE session_id = ?1 AND (text <> '' OR summary <> '')
                  ORDER BY updated_at, item_id",
            )?;
            let rows = stmt.query_map(params![session_id], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, i64>(1)? as u64,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                ))
            })?;
            let mut out = Vec::new();
            for row in rows {
                out.push(row?);
            }
            out
        };
        let mut settled = 0;
        for (item_id, turn, part, text, summary) in pending {
            // Appended last; see `docs/notes/server-store.md`.
            let stored = match part.as_str() {
                "output" => Some((
                    Kind::Text,
                    json!({"role": "assistant", "text": text, "interrupted": true,
                           "item_id": item_id, "turn": turn}),
                )),
                // The item itself: a row a kill interrupted is one a request can send.
                "reasoning" => reasoning_item(&item_id, &text, &summary).map(|item| {
                    let shown = if text.trim().is_empty() {
                        summary
                    } else {
                        text
                    };
                    (
                        Kind::Reasoning,
                        json!({"item": item, "summary": shown, "interrupted": true,
                               "item_id": item_id, "turn": turn}),
                    )
                }),
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

    /// The calls the step this session stops in left unanswered.
    ///
    /// # Invariant
    ///
    /// Read from the rows' own `(turn, step)`: a turn answers its calls before it
    /// ends, so the newest step in the transcript names the only one that can still
    /// be half-written. A call there with no result naming it is what a provider
    /// refuses to continue. Rows written before these were columns are settled by
    /// [`seal_calls`] on the open that adds them.
    ///
    /// # Invariant
    ///
    /// The newest step is the row `seq` puts last, never the largest `(turn, step)`:
    /// a turn number is counted in memory from one at every start, so a transcript
    /// that outlived a run holds numbers that go backwards. Read by number, this
    /// asked about the wrong step in exactly the transcripts it exists for -- the
    /// ones a dead run left behind.
    pub fn open_calls(&self, session_id: i64) -> anyhow::Result<Vec<(String, serde_json::Value)>> {
        let conn = self.lock();
        let mut stmt = conn.prepare(
            "SELECT c.call_id, c.display FROM messages c
               JOIN (SELECT turn, step FROM messages
                      WHERE session_id = ?1 AND turn IS NOT NULL AND step IS NOT NULL
                      ORDER BY seq DESC LIMIT 1) last
                 ON c.turn = last.turn AND c.step = last.step
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
            out.push((call_id, serde_json::from_str(&display).unwrap_or_default()));
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
        for (call_id, call) in &open {
            let payload = interrupted_result(call_id, call);
            self.append_message(session_id, Kind::ToolResult, &payload)?;
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

    /// Every operator message this session has stored, oldest first.
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
                    live_compact_interval_effective, context_generation
             FROM sessions WHERE id = ?1",
            params![session_id],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, Option<i64>>(1)?,
                    row.get::<_, Option<i64>>(2)?,
                    row.get::<_, i64>(3)?,
                ))
            },
        )?;
        Ok(crate::live_compact::Measurement {
            tokens: row.0.max(0) as u64,
            baseline_tokens: row.1.map(|tokens| tokens.max(0) as u64),
            interval: row.2.map(|tokens| tokens.max(0) as u64),
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
               AND live_compact_baseline_tokens IS NULL",
            params![input_tokens as i64, session_id, generation as i64],
        )?;
        Ok(())
    }

    /// Queues one ask, unless one is already waiting to be delivered.
    ///
    /// # Invariant
    ///
    /// The interval the ask raises rides in the payload, so a task the step boundary
    /// drops leaves the interval where it was: only a delivered ask may raise it.
    pub fn request_live_compact(
        &self,
        session_id: i64,
        session: &str,
        generation: u64,
        message: &str,
        next_interval: u64,
    ) -> anyhow::Result<bool> {
        let mut conn = self.lock();
        let tx = conn.transaction()?;
        let queued = tx.execute(
            "INSERT INTO pending_tasks (session, kind, payload, created_at)
             SELECT ?1, 'inject', ?2, ?3
              WHERE EXISTS (SELECT 1 FROM sessions
                             WHERE id = ?4 AND context_generation = ?5
                               AND live_compact_origin_seq IS NOT NULL
                               AND live_compact_baseline_tokens IS NOT NULL)
                AND NOT EXISTS (SELECT 1 FROM pending_tasks
                                 WHERE session = ?1 AND kind = 'inject'
                                   AND json_extract(payload, '$.live_compact') IS NOT NULL)",
            params![
                session,
                serde_json::to_string(&json!({
                    "session": session,
                    "message": message,
                    "live_compact": {
                        "generation": generation,
                        "next_interval": next_interval,
                    },
                }))?,
                now_millis(),
                session_id,
                generation as i64
            ],
        )?;
        tx.commit()?;
        Ok(queued > 0)
    }

    /// Opens live compaction: where the plan finished, or where the session
    /// switched the feature on.
    ///
    /// # Invariant
    ///
    /// Only the first call opens it: the origin is the permanent prefix, so a session
    /// switched off and on keeps the boundary it had. The baseline is the last
    /// persisted measurement, and a zero stays NULL for the usage write to fill.
    /// See `docs/notes/server-live-compact.md`.
    pub fn start_live_compact(&self, session_id: i64) -> anyhow::Result<()> {
        let mut conn = self.lock();
        let tx = conn.transaction()?;
        let context_tokens: i64 = tx.query_row(
            "SELECT context_tokens FROM sessions WHERE id = ?1",
            params![session_id],
            |row| row.get(0),
        )?;
        // SPEC: the origin ends a step, never a call. `docs/notes/server-live-compact.md`.
        let origin: i64 = tx.query_row(
            "SELECT COALESCE(MAX(seq), 0) FROM messages
              WHERE session_id = ?1 AND kind <> 'tool_call'",
            params![session_id],
            |row| row.get(0),
        )?;
        // Zero rows is a session that has not spoken: nothing is held back.
        tx.execute(
            "UPDATE sessions SET live_compact_origin_seq = ?2,
                    live_compact_baseline_tokens = CASE WHEN ?3 > 0 THEN ?3 ELSE NULL END
             WHERE id = ?1 AND live_compact_origin_seq IS NULL",
            params![session_id, origin, context_tokens],
        )?;
        tx.commit()?;
        Ok(())
    }

    /// Commits the checkpoint a `live_compact` call describes, as the transcript
    /// stands now.
    ///
    /// # Invariant
    ///
    /// The segment is read here, not when the ask was queued: the summary is the
    /// agent's words, and what it replaces is whatever the transcript says at the
    /// call. A call with nothing to fold answers so and appends nothing -- an error
    /// would invite the retry loop that pays for the same head twice. See
    /// `docs/notes/server-live-compact.md`.
    pub fn append_live_compact_result(
        &self,
        session_id: i64,
        call_id: &str,
        content: &serde_json::Value,
    ) -> anyhow::Result<crate::live_compact::Commit> {
        let mut conn = self.lock();
        let tx = conn.transaction()?;
        let (call_seq, call_display, call_turn, call_step) = tx.query_row(
            "SELECT seq, display, turn, step FROM messages
             WHERE session_id = ?1 AND kind = 'tool_call' AND call_id = ?2
             ORDER BY seq DESC LIMIT 1",
            params![session_id, call_id],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, Option<i64>>(2)?,
                    row.get::<_, Option<i64>>(3)?,
                ))
            },
        )?;
        let call: serde_json::Value = serde_json::from_str(&call_display)?;
        if call.get("name").and_then(serde_json::Value::as_str)
            != Some(crate::tools::live_compact::LIVE_COMPACT_TOOL)
        {
            anyhow::bail!("call {call_id} is not a live_compact call");
        }
        let bounds = crate::live_compact::bounds(&tx, session_id)?;
        let Some(segment_after_seq) = bounds.segment_after else {
            return Ok(crate::live_compact::Commit::Nothing(
                "No compaction checkpoint has been opened for this session: nothing was \
                 compacted, and the history is untouched."
                    .to_string(),
            ));
        };
        let Some(round) = crate::live_compact::ordinary_round(&tx, session_id, segment_after_seq)?
        else {
            return Ok(crate::live_compact::Commit::Nothing(
                "No completed tool round since the last checkpoint: nothing was compacted, \
                 and the history is untouched. Ask again once a step's calls have all \
                 returned."
                    .to_string(),
            ));
        };
        let (current_generation, context_tokens) = tx.query_row(
            "SELECT context_generation, context_tokens FROM sessions WHERE id = ?1",
            params![session_id],
            |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)),
        )?;
        // What the summary answers: an ask newer than the last checkpoint, or the call.
        let asked: Option<i64> = tx.query_row(
            "SELECT MAX(seq) FROM messages
              WHERE session_id = ?1 AND kind = ?2 AND seq > ?3",
            params![
                session_id,
                crate::live_compact::REQUEST_KIND,
                bounds.last_result_seq.unwrap_or(0)
            ],
            |row| row.get(0),
        )?;

        // The reasoning this step reached the call by, plus the call and its result:
        // rows a later checkpoint still has to send.
        let mut statement = tx.prepare(
            "SELECT seq FROM messages
             WHERE session_id = ?1 AND kind = 'reasoning'
               AND seq > ?2 AND seq < ?3 AND turn IS ?4 AND step IS ?5
             ORDER BY seq",
        )?;
        let rows = statement.query_map(
            params![
                session_id,
                segment_after_seq,
                call_seq,
                call_turn,
                call_step
            ],
            |row| row.get::<_, i64>(0),
        )?;
        let mut artifacts = rows.collect::<Result<Vec<_>, _>>()?;
        drop(statement);
        artifacts.push(call_seq);

        let result_seq = Self::append_message_conn(&tx, session_id, Kind::ToolResult, content)?;
        tx.execute(
            "INSERT INTO live_compact_checkpoints
             (session_id, segment_after_seq, request_seq, round_start_seq,
              cutoff_seq, result_seq, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                session_id,
                segment_after_seq,
                asked.unwrap_or(call_seq),
                round.start_seq,
                round.cutoff_seq,
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

        // `next` is cut at the next round; only `settled` accumulates. See `docs/notes/server-live-compact.md`.
        let next = call
            .get("arguments")
            .and_then(serde_json::Value::as_str)
            .and_then(|arguments| serde_json::from_str::<serde_json::Value>(arguments).ok())
            .and_then(|arguments| {
                arguments
                    .get("next")
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_string)
            })
            .unwrap_or_default();
        let next = next.trim().to_string();
        if !next.is_empty() {
            Self::append_message_conn(
                &tx,
                session_id,
                Kind::Text,
                &serde_json::json!({"role": "assistant", "text": next}),
            )?;
        }

        // The interval, the baseline and the rates all measured the replaced transcript.
        tx.execute(
            "UPDATE sessions SET live_compact_baseline_tokens = NULL,
                    live_compact_interval_effective = NULL,
                    context_tokens = 0, cached_tokens = NULL,
                    tps = NULL, ttft_ms = NULL
             WHERE id = ?1",
            params![session_id],
        )?;
        // Zero context needs an explicit +1; the trigger covers the rest.
        if context_tokens == 0 {
            tx.execute(
                "UPDATE sessions SET context_generation = context_generation + 1 WHERE id = ?1",
                params![session_id],
            )?;
        }
        tx.commit()?;
        Ok(crate::live_compact::Commit::Checkpointed(
            crate::live_compact::Committed {
                checkpoint_id,
                result_seq,
                generation: (current_generation.max(0) as u64).saturating_add(1),
            },
        ))
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

    /// Whether the model has something to answer at the end of the transcript.
    ///
    /// # Invariant
    ///
    /// Read from the row's own wire item, never from a list of kinds: the tail is half a
    /// step exactly when it is the model's own words -- a thought or an answer a stop cut
    /// short -- and a request cannot answer itself. Tool output, an error and a harness
    /// mark are what it answers, so they end a whole step. See `docs/notes/server-store.md`.
    pub fn answerable_tail(&self, session_id: i64) -> anyhow::Result<bool> {
        let conn = self.lock();
        let found: Option<(String, String)> = conn
            .query_row(
                "SELECT kind, content FROM messages
                  WHERE session_id = ?1 AND content IS NOT NULL
                  ORDER BY seq DESC LIMIT 1",
                params![session_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        let Some((kind, content)) = found else {
            // Nothing has been sent yet: the request opens on the operator's own line.
            return Ok(true);
        };
        let value: serde_json::Value =
            serde_json::from_str(&content).unwrap_or(serde_json::Value::Null);
        let said_by_model = match Kind::parse(&kind) {
            Some(Kind::Reasoning) => true,
            Some(Kind::Text | Kind::Inject) => {
                value.get("role").and_then(|role| role.as_str()) == Some("assistant")
            }
            _ => false,
        };
        Ok(!said_by_model)
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
    /// the in-flight call and its result, and its lesson lands as an injection --
    /// so an emptied transcript holds rows and no `text` one, which is what the
    /// reset keys on. A fresh session holds no rows, so the checkpoint opened
    /// before its first message survives. The reset lives here, not on `begin_turn`.
    fn append_user_conn(
        &self,
        tx: &rusqlite::Transaction<'_>,
        session_id: i64,
        expanded: &crate::meta::Expanded,
        kind: Kind,
    ) -> anyhow::Result<i64> {
        // Emptied: rows but no `text` one. Fresh: no rows, so its checkpoint stands.
        let no_text = tx.query_row(
            "SELECT EXISTS (SELECT 1 FROM messages WHERE session_id = ?1)
                 AND NOT EXISTS (SELECT 1 FROM messages
                                  WHERE session_id = ?1 AND kind = 'text')",
            params![session_id],
            |row| row.get::<_, bool>(0),
        )?;
        if no_text {
            // No text row survives: whatever stood before (handoff keeps only an
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
                        live_compact_baseline_tokens = NULL,
                        live_compact_interval_effective = NULL
                 WHERE id = ?1",
                params![session_id],
            )?;
        }
        // No origin opens here: plan finish is the only door (server-live-compact.md).
        Ok(Self::append_message_conn(
            tx,
            session_id,
            kind,
            &user_row(expanded),
        )?)
    }

    /// Accepts an idle session's first message and its turn obligation together.
    ///
    /// # Invariant
    ///
    /// Answers the task and the rows it wrote, in the order they landed: the caller
    /// publishes them before it announces where the transcript now ends. See
    /// `docs/notes/server-store.md`.
    pub fn begin_turn(
        &self,
        session_id: i64,
        session: &str,
        expanded: &crate::meta::Expanded,
    ) -> anyhow::Result<(i64, OperatorRows)> {
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
        let seq = self.append_user_conn(&tx, session_id, expanded, Kind::Text)?;
        tx.commit()?;
        Ok((
            id,
            OperatorRows {
                lines: vec![OperatorLine {
                    text: expanded.text.clone(),
                    injected: false,
                }],
                seq,
            },
        ))
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

    /// Delivers the inbox in one transaction: how many, the longest wait, the rows.
    ///
    /// # Invariant
    ///
    /// Answers the operator's lines the same way `begin_turn` does, so the caller
    /// publishes them instead of announcing a position over rows nobody drew.
    pub fn deliver_pending(
        &self,
        session_id: i64,
        session: &str,
        live_compact: bool,
    ) -> anyhow::Result<(usize, i64, OperatorRows)> {
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
        // The watermark the caller announces, and the lines it publishes first.
        let mut written = OperatorRows::default();
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

            if payload.get("live_compact").is_some() {
                // The switch is the session's and `live_compact` is the file's, for a
                // session that stated neither.
                let stated: Option<i64> = tx.query_row(
                    "SELECT live_compact FROM sessions WHERE id = ?1",
                    params![session_id],
                    |row| row.get(0),
                )?;
                if !stated.map_or(live_compact, |stated| stated != 0) {
                    tx.execute("DELETE FROM pending_tasks WHERE id = ?1", params![id])?;
                    continue;
                }
            }
            let mut next_interval = None;
            if let Some(request) = payload.get("live_compact") {
                let generation = request
                    .get("generation")
                    .and_then(serde_json::Value::as_u64)
                    .ok_or_else(|| {
                        anyhow::anyhow!("pending live compact {id} has no generation")
                    })?;
                let current_generation: i64 = tx.query_row(
                    "SELECT context_generation FROM sessions WHERE id = ?1",
                    params![session_id],
                    |row| row.get(0),
                )?;
                let after = crate::live_compact::bounds(&tx, session_id)?.segment_after;
                let round = match after {
                    Some(after) if (current_generation.max(0) as u64) == generation => {
                        crate::live_compact::ordinary_round(&tx, session_id, after)?
                    }
                    _ => None,
                };
                // An ask the boundary cannot honour is dropped, and the interval stays
                // where it was: only a delivered ask raises it.
                if round.is_none() {
                    tx.execute("DELETE FROM pending_tasks WHERE id = ?1", params![id])?;
                    continue;
                }
                next_interval = Some(
                    request
                        .get("next_interval")
                        .and_then(serde_json::Value::as_u64)
                        .ok_or_else(|| {
                            anyhow::anyhow!("pending live compact {id} has no next interval")
                        })?,
                );
            }

            let expanded = crate::meta::Expanded::from_json(&payload)
                .ok_or_else(|| anyhow::anyhow!("pending inject {id} has no message"))?;
            written.seq = self.append_user_conn(&tx, session_id, &expanded, Kind::Inject)?;
            written.lines.push(OperatorLine {
                text: expanded.text.clone(),
                injected: true,
            });
            if let Some(next) = next_interval {
                tx.execute(
                    "UPDATE sessions SET live_compact_interval_effective = ?1 WHERE id = ?2",
                    params![i64::try_from(next).unwrap_or(i64::MAX), session_id],
                )?;
                Self::append_message_conn(
                    &tx,
                    session_id,
                    Kind::LiveCompactRequest,
                    &serde_json::json!({}),
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
        Ok((delivered, waited, written))
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
                "UPDATE sessions SET monitor_seq = ?2 WHERE id = ?1",
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

    /// Drops everything of one session except the rows of `kind`.
    ///
    /// # Invariant
    ///
    /// The inbox (`inject`) is received input and survives every stop: what a stop
    /// clears is what the session no longer owes, an idle session's leftover `turn`
    /// row. Only a delete, which takes the whole session, clears without an exception.
    pub fn clear_pending_tasks_except(&self, session: &str, kind: &str) -> anyhow::Result<usize> {
        Ok(self.lock().execute(
            "DELETE FROM pending_tasks WHERE session = ?1 AND kind <> ?2",
            params![session, kind],
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

    /// Records a channel this session started: its name, and where it lives.
    pub fn add_channel(
        &self,
        session: &str,
        path: &str,
        program: &str,
        remote: Option<&str>,
        // The machine's own spelling: a peer's directory is not a path this one has.
        dir: &str,
        pid: u32,
        // The relay's own pid and its start, when this machine started the relay. A
        // successor cannot adopt what it cannot name: `pid` is the program's, and
        // the relay is reachable only as the parent of a pid that may be gone.
        relay: Option<(u32, i64)>,
    ) -> anyhow::Result<i64> {
        let conn = self.lock();
        conn.execute(
            "INSERT INTO channels (session, path, program, remote, dir, pid, started_at, \
             relay_pid, relay_started_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                session,
                path,
                program,
                remote,
                dir,
                i64::from(pid),
                now_millis(),
                relay.map(|(pid, _)| i64::from(pid)),
                relay.map(|(_, at)| at),
            ],
        )?;
        Ok(conn.last_insert_rowid())
    }

    /// Every channel a session has: one it started, whose program still runs.
    pub fn channels_of(&self, session: &str) -> anyhow::Result<Vec<ChannelRow>> {
        let conn = self.lock();
        let mut stmt = conn.prepare(
            "SELECT id, session, path, program, remote, dir, pid, started_at, upto, wrote_at, wait_marker, until_sentinel, detached, relay_pid, relay_started_at
             FROM channels WHERE session = ?1 ORDER BY path",
        )?;
        let rows = stmt
            .query_map(params![session], channel_row)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    /// Every channel, across sessions: what a watcher has to look at.
    ///
    /// # Invariant
    ///
    /// There is no other kind of row: a channel whose program has ended is removed where
    /// the end is seen, so a name never points at a program that is gone.
    pub fn every_channel(&self) -> anyhow::Result<Vec<ChannelRow>> {
        let conn = self.lock();
        let mut stmt = conn.prepare(
            "SELECT id, session, path, program, remote, dir, pid, started_at, upto, wrote_at, wait_marker, until_sentinel, detached, relay_pid, relay_started_at
             FROM channels ORDER BY id",
        )?;
        let rows = stmt
            .query_map([], channel_row)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    /// One channel of one session, by the name it is called.
    pub fn channel_of(&self, session: &str, path: &str) -> anyhow::Result<Option<ChannelRow>> {
        let conn = self.lock();
        let mut stmt = conn.prepare(
            "SELECT id, session, path, program, remote, dir, pid, started_at, upto, wrote_at, wait_marker, until_sentinel, detached, relay_pid, relay_started_at
             FROM channels WHERE session = ?1 AND path = ?2",
        )?;
        let mut rows = stmt.query(params![session, path])?;
        match rows.next()? {
            Some(row) => Ok(Some(channel_row(row)?)),
            None => Ok(None),
        }
    }

    /// Where this reader has read to: taking the window is what advances it.
    pub fn set_channel_upto(&self, id: i64, upto: u64) -> anyhow::Result<()> {
        self.lock().execute(
            "UPDATE channels SET upto = ?2 WHERE id = ?1",
            params![id, i64::try_from(upto).unwrap_or(i64::MAX)],
        )?;
        Ok(())
    }

    /// Records that this channel was written to: what a call's `time` is measured
    /// from. See `docs/notes/server-channel.md`.
    pub fn set_channel_wrote(&self, session: &str, path: &str, at: i64) -> anyhow::Result<()> {
        let conn = self.lock();
        conn.execute(
            "UPDATE channels SET wrote_at = ?3 WHERE session = ?1 AND path = ?2",
            params![session, path, at],
        )?;
        Ok(())
    }

    /// Removes a channel's row, and says whether this was the telling.
    ///
    /// # Invariant
    ///
    /// The row goes with the program: SPEC's channel is a program, so a row outliving it
    /// is a name that answers for a process that is gone. The DELETE is the claim -- two
    /// writers may see one end, and only the one that removed the row may speak of it.
    pub fn claim_channel_end(&self, id: i64) -> anyhow::Result<bool> {
        Ok(self
            .lock()
            .execute("DELETE FROM channels WHERE id = ?1", params![id])?
            > 0)
    }

    /// Records that a caller waits on this channel, and for what.
    pub fn set_channel_wait(&self, session: &str, path: &str, marker: &str) -> anyhow::Result<()> {
        self.lock().execute(
            "UPDATE channels SET wait_marker = ?3 WHERE session = ?1 AND path = ?2",
            params![session, path, marker],
        )?;
        Ok(())
    }

    /// Remembers the marker this write ends its reply with: SPEC's sticky util.
    pub fn set_channel_sentinel(
        &self,
        session: &str,
        path: &str,
        sentinel: &str,
    ) -> anyhow::Result<()> {
        self.lock().execute(
            "UPDATE channels SET until_sentinel = ?3 WHERE session = ?1 AND path = ?2",
            params![session, path, sentinel],
        )?;
        Ok(())
    }

    /// Marks every channel of every session as one a `cancel` was asked to keep.
    ///
    /// # Invariant
    ///
    /// A state, not an argument: the flag outlives the stop that wrote it, so a
    /// later stop that asked for nothing spares these too. This is the whole of
    /// SPEC's "make every channel detached" -- a row that carries it is one the next
    /// start may adopt.
    pub fn mark_detached(&self) -> anyhow::Result<usize> {
        Ok(self
            .lock()
            .execute("UPDATE channels SET detached = 1", [])?)
    }

    /// Clears the flag on one channel: the relay was adopted, or the channel ended.
    ///
    /// # Invariant
    ///
    /// Adoption is what clears it, so a second start does not offer the same relay
    /// again. A row that ends takes its flag with it either way.
    pub fn clear_detached(&self, id: i64) -> anyhow::Result<()> {
        self.lock().execute(
            "UPDATE channels SET detached = 0 WHERE id = ?1",
            params![id],
        )?;
        Ok(())
    }

    /// The channels a start may adopt: a `cancel` marked them, and this is where the
    /// relay's identity lives.
    pub fn detached_channels(&self) -> anyhow::Result<Vec<ChannelRow>> {
        let conn = self.lock();
        let mut stmt = conn.prepare(
            "SELECT id, session, path, program, remote, dir, pid, started_at, upto, wrote_at, \
             wait_marker, until_sentinel, detached, relay_pid, relay_started_at
             FROM channels WHERE detached = 1 ORDER BY id",
        )?;
        let rows = stmt
            .query_map([], channel_row)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    /// Clears the flag, and says whether this caller cleared it: one delivery, claimed.
    pub fn take_channel_wait(&self, id: i64) -> anyhow::Result<bool> {
        Ok(self.lock().execute(
            "UPDATE channels SET wait_marker = NULL WHERE id = ?1 AND wait_marker IS NOT NULL",
            params![id],
        )? > 0)
    }

    /// Gives a channel another name. The directory does not move: the name is a key
    /// of this table, not a place on disk.
    pub fn rename_channel(&self, session: &str, path: &str, new_path: &str) -> anyhow::Result<()> {
        self.lock().execute(
            "UPDATE channels SET path = ?3 WHERE session = ?1 AND path = ?2",
            params![session, path, new_path],
        )?;
        Ok(())
    }

    /// Forgets one channel: the program is gone and nothing it said is left to read.
    pub fn delete_channel(&self, session: &str, path: &str) -> anyhow::Result<()> {
        self.lock().execute(
            "DELETE FROM channels WHERE session = ?1 AND path = ?2",
            params![session, path],
        )?;
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
        let action = match action {
            Some(action) => Some(serde_json::to_string(&action)?),
            None => None,
        };
        let conn = self.lock();
        // One name is one subscription: arming replaces the row it held.
        conn.execute(
            "DELETE FROM subscriptions WHERE session = ?1 AND name = ?2",
            params![session, name],
        )?;
        conn.execute(
            "INSERT INTO subscriptions
                 (session, name, pattern, persistent, action, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![session, name, pattern, persistent, action, now_millis()],
        )?;
        Ok(SubscriptionRow {
            id: conn.last_insert_rowid(),
            session: session.to_string(),
            name: name.to_string(),
            pattern: pattern.to_string(),
            persistent,
            action,
        })
    }

    /// Every armed subscription, oldest first, so a matcher can keep one list.
    pub fn subscriptions(&self) -> anyhow::Result<Vec<SubscriptionRow>> {
        let conn = self.lock();
        let mut stmt = conn.prepare(
            "SELECT id, session, name, pattern, persistent, action
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
                    action: r.get(5)?,
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
    /// The cmd a match runs, as `{"name":…,"args":{…}}`, when it carries one.
    pub action: Option<String>,
}

/// The operator's own lines a transaction wrote, and where the transcript ended.
///
/// # Invariant
///
/// One publisher for both live and replayed rows: `commands::row_event` draws such
/// a row as `ResponsePart::Input`, so a caller publishes these lines and only then
/// announces `seq`. A window reads the announcement as "this row is drawn".
#[derive(Debug, Clone, Default)]
pub struct OperatorRows {
    /// The lines, in the order they landed.
    pub lines: Vec<OperatorLine>,
    /// The seq of the last row this transaction wrote.
    pub seq: i64,
}

/// One line a transaction wrote, and whose words they are.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OperatorLine {
    pub text: String,
    /// The harness's own words: drawn as its own block, never as the operator's.
    pub injected: bool,
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

/// One channel: the name a session calls it by, and what became of its program.
///
/// # Invariant
///
/// `path` is this table's key and `dir` is where the bytes are: the name is not a
/// place on disk, so a rename moves one column and nothing else. See
/// `docs/notes/server-channel.md`.
#[derive(Debug, Clone)]
pub struct ChannelRow {
    pub id: i64,
    pub session: String,
    pub path: String,
    /// The program it runs, as it was started: a name, not a path.
    pub program: String,
    /// The machine it runs on, by the name the operator gave it; `None` is this one.
    pub remote: Option<String>,
    pub dir: String,
    pub pid: i64,
    pub started_at: i64,
    /// How far this reader has read: taking the window is what advances it.
    pub upto: u64,
    /// When this channel was last written to; `None` before the first write.
    pub wrote_at: Option<i64>,
    /// What a caller waits for: the marker, `Some("")` for the end alone. `None` is
    /// nobody waiting.
    pub wait_marker: Option<String>,
    /// The marker this channel's last write ended its reply with; `''` before one.
    pub until_sentinel: String,
    /// Whether a `cancel(keep_alive)` asked for this channel to outlive the stop.
    ///
    /// # Invariant
    ///
    /// A state, not an argument: it survives the stop that set it, so a later stop
    /// that did not ask spares it too. `stop_every` reads this; nothing else writes
    /// it but `mark_detached` and `clear_detached`.
    pub detached: bool,
    /// The relay's pid, which is not [`Self::pid`]: that one is the program's.
    /// `None` is a row written before the column, or a channel on a peer.
    pub relay_pid: Option<i64>,
    /// When that relay started, so a recycled pid is refused before a job is armed.
    pub relay_started_at: Option<i64>,
}

fn channel_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<ChannelRow> {
    Ok(ChannelRow {
        id: row.get(0)?,
        session: row.get(1)?,
        path: row.get(2)?,
        program: row.get(3)?,
        remote: row.get(4)?,
        dir: row.get(5)?,
        pid: row.get(6)?,
        started_at: row.get(7)?,
        upto: row.get::<_, i64>(8)?.max(0) as u64,
        wrote_at: row.get(9)?,
        wait_marker: row.get(10)?,
        until_sentinel: row.get(11)?,
        detached: row.get::<_, i64>(12)? != 0,
        relay_pid: row.get(13)?,
        relay_started_at: row.get(14)?,
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
    /// What this session is for: a read, or the work it may change.
    pub mode: crate::explore::Mode,
    /// The session's own live compact switch; `None` follows the home's `[agent]`.
    pub live_compact_enabled: Option<bool>,
    /// Its own interval; `None` follows the home's `[agent]`.
    pub live_compact_interval_tokens: Option<u64>,
    /// The branch, and so the worktree, this session was born in; `None` made none.
    pub worktree: Option<String>,
    /// Whether that worktree's commits are all in the main line; `None` is unread.
    pub worktree_merged: Option<bool>,
    /// Whether it holds anything the main line does not; `None` is unread.
    pub worktree_changed: Option<bool>,
    /// The session this row is a disposable dream of; `None` is an ordinary one.
    pub dream_of: Option<i64>,
    /// The checkpoint generation this row's dreaming concerns; `None` never dreamed.
    pub dreamed_checkpoint_id: Option<i64>,
}

/// A dream clone: keep_alive off, live compact explicitly off, mode `work`.
/// See `docs/notes/server-dream.md`.
#[derive(Debug, Clone, Copy)]
pub struct Dream {
    pub source: i64,
    pub generation: i64,
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
    /// The last row a monitor turn was given.
    pub monitor_seq: i64,
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
    /// The request this row answered, within its turn: 0 for a row no step wrote.
    pub step: Option<u64>,
    /// A `tool_result`'s verdict, as the turn it belongs to recorded it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ok: Option<bool>,
    /// How long that tool ran, in milliseconds.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub elapsed_ms: Option<u64>,
}

/// What one row of the transcript is, as a closed set.
///
/// # Invariant
///
/// Matched exhaustively wherever a row is decided about -- what a request sends,/// what a reader draws -- so a new kind is a build error at each. It was a `&str`,/// and the kind that reached a wildcard stored a message the model never got.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Kind {
    Text,
    Reasoning,
    ToolCall,
    ToolResult,
    Notice,
    ThinkingMark,
    Inject,
    Error,
    System,
    Tools,
    BuiltinCall,
    LiveCompactRequest,
}

impl Kind {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Kind::Text => "text",
            Kind::Reasoning => "reasoning",
            Kind::ToolCall => "tool_call",
            Kind::ToolResult => "tool_result",
            Kind::Notice => "notice",
            Kind::ThinkingMark => "thinking_mark",
            Kind::Inject => "inject",
            Kind::Error => "error",
            Kind::System => "system",
            Kind::Tools => "tools",
            Kind::BuiltinCall => "builtin_call",
            Kind::LiveCompactRequest => crate::live_compact::REQUEST_KIND,
        }
    }

    /// The variant a stored kind names; `None` is a row this build cannot name.
    pub(crate) fn parse(kind: &str) -> Option<Self> {
        match kind {
            "text" => Some(Kind::Text),
            "reasoning" => Some(Kind::Reasoning),
            "tool_call" => Some(Kind::ToolCall),
            "tool_result" => Some(Kind::ToolResult),
            "notice" => Some(Kind::Notice),
            "thinking_mark" => Some(Kind::ThinkingMark),
            "inject" => Some(Kind::Inject),
            "error" => Some(Kind::Error),
            "system" => Some(Kind::System),
            "tools" => Some(Kind::Tools),
            "builtin_call" => Some(Kind::BuiltinCall),
            other if other == crate::live_compact::REQUEST_KIND => Some(Kind::LiveCompactRequest),
            _ => None,
        }
    }
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
            step: value.get("step").and_then(|v| v.as_u64()),
            ok: None,
            elapsed_ms: None,
        };
        match Kind::parse(kind) {
            Some(Kind::Text) => {
                row.role = str_of("role");
                row.text = str_of("text").unwrap_or_default();
            }
            Some(Kind::Reasoning) => row.text = str_of("summary").unwrap_or_default(),
            Some(Kind::ToolCall) => {
                row.call_id = str_of("call_id");
                row.name = str_of("name");
                row.text = str_of("arguments").unwrap_or_default();
            }
            Some(Kind::ToolResult) => {
                row.call_id = str_of("call_id");
                row.text = str_of("output").unwrap_or_default();
                // Absent on a row written before the outcome was kept: a read
                // of an old transcript then says nothing, not a guess.
                row.ok = value.get("ok").and_then(|v| v.as_bool());
                row.elapsed_ms = value.get("elapsed_ms").and_then(|v| v.as_u64());
            }
            // The harness talking: a notice to the reader alone, an error to both.
            Some(Kind::Notice | Kind::ThinkingMark | Kind::Inject | Kind::Error) => {
                row.text = str_of("text").unwrap_or_default()
            }
            // The reader sees which prompt was in force, not 10k characters of it.
            Some(Kind::System) => {
                let cwd = str_of("cwd").unwrap_or_default();
                let chars = str_of("instructions").unwrap_or_default().chars().count();
                row.text = format!(
                    "[prompt in force] {chars} characters of instructions, read from {cwd}"
                );
            }
            // The set in force: how big it is, not the schema the model reads.
            Some(Kind::Tools) => {
                let count = value
                    .get("tools")
                    .and_then(|v| v.as_array())
                    .map(|list| list.len())
                    .unwrap_or(0);
                row.text = format!("tool set in force: {count} tool(s)");
            }
            Some(Kind::LiveCompactRequest) => row.text = "live compact requested".to_string(),
            // Something that ran at the provider, not here: it has no output to
            // show, so the line describes what it did instead.
            Some(Kind::BuiltinCall) => {
                let item = value.get("item").unwrap_or(&serde_json::Value::Null);
                row.text = activity_brief(item);
            }
            None => tracing::warn!("unknown message kind {kind}"),
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
/// The item an interrupted thought is sent back as.
///
/// # Invariant
///
/// The text goes into the field its deltas came from -- `content` with `reasoning_text`
/// for the thought itself, `summary` with `summary_text` for a provider's own summary --
/// so what a killed stream left on disk is already a sendable item. Guessing the field
/// is what made a resumed turn re-think its whole step. See `docs/notes/server-store.md`.
pub(crate) fn reasoning_item(
    item_id: &str,
    text: &str,
    summary: &str,
) -> Option<serde_json::Value> {
    let mut item = json!({"type": "reasoning"});
    if !text.trim().is_empty() {
        item["content"] = json!([{"type": "reasoning_text", "text": text}]);
    }
    if !summary.trim().is_empty() {
        item["summary"] = json!([{"type": "summary_text", "text": summary}]);
    }
    if item.get("content").is_none() && item.get("summary").is_none() {
        return None;
    }
    if !item_id.is_empty() {
        item["id"] = json!(item_id);
    }
    Some(item)
}

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

fn wire_item(kind: Kind, value: &serde_json::Value) -> Option<serde_json::Value> {
    let text_of = |key: &str| {
        value
            .get(key)
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string()
    };
    // Pictures stay in `display` and resolve per request.
    match kind {
        // Records a request reads, not messages it sends: the prompt and the tool
        // set in force keep their payload in `display`.
        Kind::System | Kind::Tools => None,
        // An injection is a user turn to the model: the kind names the reader's block.
        Kind::Text | Kind::Inject => {
            let role = text_of("role");
            Some(if role == "assistant" {
                json!({"role": "assistant", "content": [{"type": "output_text", "text": text_of("text")}]})
            } else {
                json!({"role": role, "content": text_of("text")})
            })
        }
        // Sent as well as shown: the session that failed reads why. `server-agent.md`.
        Kind::Error => Some(json!({
            "role": "user",
            "content": crate::prompts::harness_wrap(text_of("text")),
        })),
        Kind::Reasoning => {
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
        Kind::ToolCall => Some(json!({
            "type": "function_call",
            "call_id": text_of("call_id"),
            "name": text_of("name"),
            "arguments": text_of("arguments"),
        })),
        // A boundary the harness draws, not a thing the reader said: see
        // `docs/notes/server-store.md`.
        Kind::ThinkingMark => Some(json!({"role": "user", "content": text_of("text")})),
        Kind::ToolResult => Some(json!({
            "type": "function_call_output",
            "call_id": text_of("call_id"),
            "output": text_of("output"),
        })),
        Kind::BuiltinCall => {
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
        // Records a request reads rather than messages it sends: the harness.
        Kind::Notice | Kind::LiveCompactRequest => None,
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
