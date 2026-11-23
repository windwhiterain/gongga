//! Memory checkpoints and their visibility views.
//! See `SPEC.md` "Memory Mask".

use anyhow::{Context, anyhow, bail};
use rusqlite::{Connection, OptionalExtension, params};
use serde_json::Value;

use crate::store::Kind;

const MASK: &str = "mask";
const UNMASK: &str = "unmask";
const MEMORIZE_CAUSE: &str = "memorize";
const RECALL_CAUSE: &str = "recall";

/// A committed memory checkpoint. The source range is `(source_after_seq, cutoff_seq]`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Memorized {
    pub id: String,
    pub output: String,
    pub checkpoint_id: i64,
    pub source_after_seq: i64,
    pub cutoff_seq: i64,
    pub result_seq: i64,
    pub view_generation: u64,
}

/// What a `memorize` call did: committed a checkpoint, or explained why not.
///
/// # Invariant
///
/// `Nothing` is successful and writes no row, matching live compaction's contract.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MemorizeCommit {
    Checkpointed(Memorized),
    Nothing(String),
}

/// The visibility changes and tool-result position committed by one `recall` call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemoryRecall {
    pub output: String,
    pub result_seq: i64,
    pub recalled: Vec<String>,
    pub auto_masked: Vec<String>,
    pub view_generation: u64,
}

/// The visibility changes committed by enforcing a configured unmasked-block count.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LimitEnforced {
    pub auto_masked: Vec<String>,
    pub view_generation: u64,
}

/// The stable public identifier of a memory checkpoint.
pub fn public_id(session_id: i64, checkpoint_id: i64) -> String {
    format!("mem:{session_id}:{checkpoint_id}")
}

/// Adds memory masks after live compaction has created its checkpoint tables.
pub fn migrate(conn: &Connection) -> rusqlite::Result<()> {
    if conn
        .prepare("SELECT kind FROM live_compact_checkpoints LIMIT 1")
        .is_err()
    {
        conn.execute(
            "ALTER TABLE live_compact_checkpoints
             ADD COLUMN kind TEXT NOT NULL DEFAULT 'cut'",
            [],
        )?;
    }
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS memory_state (
            session_id          INTEGER PRIMARY KEY REFERENCES sessions(id) ON DELETE CASCADE,
            view_generation     INTEGER NOT NULL DEFAULT 0 CHECK (view_generation >= 0),
            next_unmasked_order INTEGER NOT NULL DEFAULT 0 CHECK (next_unmasked_order >= 0),
            unmasked_count      INTEGER NOT NULL DEFAULT 0 CHECK (unmasked_count >= 0),
            unmasked_tokens     INTEGER CHECK (unmasked_tokens IS NULL OR unmasked_tokens >= 0)
        );

        CREATE TABLE IF NOT EXISTS memory_blocks (
            checkpoint_id   INTEGER PRIMARY KEY REFERENCES live_compact_checkpoints(id) ON DELETE CASCADE,
            session_id      INTEGER NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
            source_after_seq INTEGER NOT NULL,
            cutoff_seq      INTEGER NOT NULL,
            summary         TEXT NOT NULL,
            token_count     INTEGER CHECK (token_count IS NULL OR token_count >= 0),
            unmasked_order  INTEGER,
            created_at      INTEGER NOT NULL,
            CHECK (unmasked_order IS NULL OR unmasked_order >= 0)
        );
        CREATE INDEX IF NOT EXISTS memory_blocks_by_session
            ON memory_blocks(session_id, checkpoint_id);
        CREATE UNIQUE INDEX IF NOT EXISTS memory_blocks_unmasked_fifo
            ON memory_blocks(session_id, unmasked_order)
            WHERE unmasked_order IS NOT NULL;

        CREATE TABLE IF NOT EXISTS memory_view_events (
            id            INTEGER PRIMARY KEY,
            session_id    INTEGER NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
            checkpoint_id INTEGER NOT NULL REFERENCES memory_blocks(checkpoint_id) ON DELETE CASCADE,
            action        TEXT NOT NULL CHECK (action IN ('mask', 'unmask')),
            cause         TEXT NOT NULL,
            generation    INTEGER NOT NULL CHECK (generation >= 0),
            created_at    INTEGER NOT NULL
        );
        CREATE INDEX IF NOT EXISTS memory_view_events_by_session
            ON memory_view_events(session_id, id);
        CREATE INDEX IF NOT EXISTS memory_view_events_by_checkpoint
            ON memory_view_events(checkpoint_id, id);

        CREATE TRIGGER IF NOT EXISTS memory_blocks_count_insert
        AFTER INSERT ON memory_blocks
        WHEN NEW.unmasked_order IS NOT NULL
        BEGIN
            UPDATE memory_state
               SET unmasked_count = unmasked_count + 1
             WHERE session_id = NEW.session_id;
        END;
        CREATE TRIGGER IF NOT EXISTS memory_blocks_count_delete
        AFTER DELETE ON memory_blocks
        WHEN OLD.unmasked_order IS NOT NULL
        BEGIN
            UPDATE memory_state
               SET unmasked_count = unmasked_count - 1
             WHERE session_id = OLD.session_id;
        END;
        CREATE TRIGGER IF NOT EXISTS memory_blocks_count_mask
        AFTER UPDATE OF unmasked_order ON memory_blocks
        WHEN OLD.unmasked_order IS NOT NULL AND NEW.unmasked_order IS NULL
        BEGIN
            UPDATE memory_state
               SET unmasked_count = unmasked_count - 1
             WHERE session_id = OLD.session_id;
        END;
        CREATE TRIGGER IF NOT EXISTS memory_blocks_count_unmask
        AFTER UPDATE OF unmasked_order ON memory_blocks
        WHEN OLD.unmasked_order IS NULL AND NEW.unmasked_order IS NOT NULL
        BEGIN
            UPDATE memory_state
               SET unmasked_count = unmasked_count + 1
             WHERE session_id = NEW.session_id;
        END;",
    )?;
    if conn
        .prepare("SELECT token_count FROM memory_blocks LIMIT 1")
        .is_err()
    {
        conn.execute(
            "ALTER TABLE memory_blocks ADD COLUMN token_count INTEGER",
            [],
        )?;
    }
    if conn
        .prepare("SELECT unmasked_tokens FROM memory_state LIMIT 1")
        .is_err()
    {
        conn.execute(
            "ALTER TABLE memory_state ADD COLUMN unmasked_tokens INTEGER",
            [],
        )?;
    }
    Ok(())
}

#[derive(Debug)]
pub(crate) struct StoredCall {
    pub(crate) seq: i64,
    pub(crate) turn: Option<i64>,
    pub(crate) step: Option<i64>,
    pub(crate) arguments: Value,
}

#[derive(Debug, Clone, Copy)]
struct State {
    view_generation: i64,
    next_unmasked_order: i64,
    unmasked_count: i64,
}

#[derive(Debug)]
struct RequestedBlock {
    checkpoint_id: i64,
    id: String,
    unmasked: bool,
    tokens: Option<i64>,
}

pub(crate) fn stored_call(
    conn: &Connection,
    session_id: i64,
    call_id: &str,
    expected_tool: &str,
) -> anyhow::Result<StoredCall> {
    let (seq, display, turn, step) = conn.query_row(
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
    let display: Value = serde_json::from_str(&display)
        .with_context(|| format!("could not read tool call {call_id}"))?;
    let arguments = unwrap_call(display, call_id, expected_tool)?;
    Ok(StoredCall {
        seq,
        turn,
        step,
        arguments,
    })
}

/// The effective arguments of a stored call, read through the `gongga_cli` envelope.
///
/// # Invariant
///
/// SPEC: `<tool>(**args) == gongga_cli(Cmd{name: <tool>})`. A call naming
/// `expected` in `cmd` is `expected`'s call, its arguments `cmd.args`. The loop
/// stores the call as the model spelled it; the command reads the envelope here.
pub(crate) fn unwrap_call(display: Value, call_id: &str, expected: &str) -> anyhow::Result<Value> {
    let name = display
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let arguments = display
        .get("arguments")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("call {call_id} has no encoded arguments"))?;
    let arguments: Value = serde_json::from_str(arguments)
        .with_context(|| format!("could not read arguments of call {call_id}"))?;
    let (name, arguments) = if name == "gongga_cli" {
        let cmd = arguments.get("cmd").cloned().unwrap_or(Value::Null);
        let inner = cmd
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        let args = cmd
            .get("args")
            .cloned()
            .unwrap_or_else(|| serde_json::json!({}));
        (inner, args)
    } else {
        (name, arguments)
    };
    if name != expected {
        bail!("call {call_id} is not a {expected} call");
    }
    Ok(arguments)
}

fn checkpoint_from_public_id(session_id: i64, id: &str) -> anyhow::Result<i64> {
    let mut parts = id.split(':');
    let (Some("mem"), Some(session), Some(checkpoint), None) =
        (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        bail!("invalid memory id {id:?}");
    };
    let stated_session = session
        .parse::<i64>()
        .with_context(|| format!("invalid memory id {id:?}"))?;
    if stated_session != session_id {
        bail!("memory id {id:?} does not belong to session {session_id}");
    }
    let checkpoint_id = checkpoint
        .parse::<i64>()
        .with_context(|| format!("invalid memory id {id:?}"))?;
    if public_id(session_id, checkpoint_id) != id {
        bail!("memory id {id:?} is not in canonical mem:<session>:<checkpoint> form");
    }
    Ok(checkpoint_id)
}

fn ensure_state(conn: &Connection, session_id: i64) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT INTO memory_state (session_id) VALUES (?1)
         ON CONFLICT(session_id) DO NOTHING",
        params![session_id],
    )?;
    Ok(())
}

fn read_state(conn: &Connection, session_id: i64) -> rusqlite::Result<State> {
    conn.query_row(
        "SELECT view_generation, next_unmasked_order, unmasked_count
         FROM memory_state WHERE session_id = ?1",
        params![session_id],
        |row| {
            Ok(State {
                view_generation: row.get(0)?,
                next_unmasked_order: row.get(1)?,
                unmasked_count: row.get(2)?,
            })
        },
    )
}

fn nonnegative(value: i64, what: &str) -> anyhow::Result<u64> {
    u64::try_from(value).map_err(|_| anyhow!("{what} is negative: {value}"))
}

fn advance_generation(conn: &Connection, session_id: i64) -> anyhow::Result<i64> {
    let current = read_state(conn, session_id)?.view_generation;
    let next = current
        .checked_add(1)
        .ok_or_else(|| anyhow!("memory view generation overflow for session {session_id}"))?;
    conn.execute(
        "UPDATE memory_state SET view_generation = ?2 WHERE session_id = ?1",
        params![session_id, next],
    )?;
    Ok(next)
}

fn record_view_event(
    conn: &Connection,
    session_id: i64,
    checkpoint_id: i64,
    action: &str,
    cause: &str,
    generation: i64,
) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT INTO memory_view_events
         (session_id, checkpoint_id, action, cause, generation, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![
            session_id,
            checkpoint_id,
            action,
            cause,
            generation,
            crate::store::now_millis()
        ],
    )?;
    Ok(())
}

fn maximum(value: u64) -> anyhow::Result<i64> {
    i64::try_from(value).map_err(|_| anyhow!("max_unmasked_blocks exceeds SQLite integer range"))
}

fn projected_unmasked_tokens(conn: &Connection, session_id: i64) -> rusqlite::Result<Option<i64>> {
    conn.query_row(
        "SELECT unmasked_tokens FROM memory_state WHERE session_id = ?1",
        params![session_id],
        |row| row.get(0),
    )
}

/// Masks the FIFO head until the configured count holds, at one pinned generation.
///
/// # Invariant
///
/// The count is maintained by triggers and every head lookup uses the partial FIFO index.
fn enforce_limit_at_generation(
    conn: &Connection,
    session_id: i64,
    maximum_tokens: Option<i64>,
    mut unmasked_tokens: Option<i64>,
    cause: &str,
    generation: i64,
) -> anyhow::Result<Vec<String>> {
    let mut auto_masked = Vec::new();
    loop {
        let count = read_state(conn, session_id)?.unmasked_count;
        let over_tokens = maximum_tokens
            .is_some_and(|limit| unmasked_tokens.unwrap_or(0).saturating_sub(limit) > 0);
        if !over_tokens || count == 0 {
            break;
        }
        let (head, tokens) = conn
            .query_row(
                "SELECT checkpoint_id, token_count FROM memory_blocks
                 WHERE session_id = ?1 AND unmasked_order IS NOT NULL
                 ORDER BY unmasked_order ASC LIMIT 1",
                params![session_id],
                |row| Ok((row.get::<_, i64>(0)?, row.get::<_, Option<i64>>(1)?)),
            )
            .optional()?
            .ok_or_else(|| {
                anyhow!(
                    "memory unmasked count has no corresponding FIFO head for session {session_id}"
                )
            })?;
        let changed = conn.execute(
            "UPDATE memory_blocks SET unmasked_order = NULL
             WHERE session_id = ?1 AND checkpoint_id = ?2 AND unmasked_order IS NOT NULL",
            params![session_id, head],
        )?;
        if changed != 1 {
            bail!("memory FIFO head changed while enforcing session {session_id}'s limit");
        }
        if let (Some(visible), Some(tokens)) = (unmasked_tokens.as_mut(), tokens) {
            *visible = visible.saturating_sub(tokens).max(0);
        }
        record_view_event(conn, session_id, head, MASK, cause, generation)?;
        auto_masked.push(public_id(session_id, head));
    }
    if maximum_tokens.is_some() {
        conn.execute(
            "UPDATE memory_state SET unmasked_tokens = ?2 WHERE session_id = ?1",
            params![session_id, unmasked_tokens],
        )?;
    }
    let remaining = read_state(conn, session_id)?.unmasked_count;
    let token_limit_unmet =
        remaining > 0 && maximum_tokens.is_some_and(|limit| unmasked_tokens.unwrap_or(0) > limit);
    if token_limit_unmet {
        bail!("memory visible usage did not reach the configured limit");
    }
    Ok(auto_masked)
}

impl crate::store::Store {
    /// Commits a mask checkpoint, its visible summary artifact, and its initial masked view.
    ///
    /// # Invariant
    ///
    /// The transcript is append-only: only the memory view advances, never
    /// `context_generation`.
    pub fn memorize(
        &self,
        session_id: i64,
        call_id: &str,
        content: &Value,
    ) -> anyhow::Result<MemorizeCommit> {
        let connection = self.picture_conn();
        let mut connection = connection.lock().unwrap_or_else(|error| error.into_inner());
        let tx = connection.transaction()?;
        let call = stored_call(
            &tx,
            session_id,
            call_id,
            crate::tools::memory::MEMORIZE_TOOL,
        )?;
        let summary = call
            .arguments
            .get("summary")
            .and_then(Value::as_str)
            .filter(|summary| !summary.trim().is_empty())
            .ok_or_else(|| anyhow!("memorize requires a non-empty summary"))?;

        let bounds = crate::live_compact::bounds(&tx, session_id)?;
        let Some(segment_after_seq) = bounds.segment_after else {
            return Ok(MemorizeCommit::Nothing(
                "No compaction checkpoint has been opened for this session: nothing was \
                 memorized, and the history and memory view are untouched."
                    .to_string(),
            ));
        };
        let Some(round) = crate::live_compact::ordinary_round(&tx, session_id, segment_after_seq)?
        else {
            return Ok(MemorizeCommit::Nothing(
                "No completed tool round since the last checkpoint: nothing was memorized, \
                 and the history and memory view are untouched. Ask again once a step's calls \
                 have all returned."
                    .to_string(),
            ));
        };
        let requested: Option<i64> = tx.query_row(
            "SELECT MAX(seq) FROM messages
             WHERE session_id = ?1 AND kind = ?2 AND seq > ?3",
            params![
                session_id,
                crate::live_compact::REQUEST_KIND,
                bounds.last_result_seq.unwrap_or(0)
            ],
            |row| row.get(0),
        )?;

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
                call.seq,
                call.turn,
                call.step
            ],
            |row| row.get::<_, i64>(0),
        )?;
        let mut artifacts = rows.collect::<Result<Vec<_>, _>>()?;
        drop(statement);
        artifacts.push(call.seq);

        ensure_state(&tx, session_id)?;
        tx.execute(
            "UPDATE memory_state SET unmasked_tokens = NULL WHERE session_id = ?1",
            params![session_id],
        )?;
        // The id needs the checkpoint row. Its result_seq placeholder is transaction-local.
        tx.execute(
            "INSERT INTO live_compact_checkpoints
             (session_id, segment_after_seq, request_seq, round_start_seq,
              cutoff_seq, result_seq, created_at, kind)
             VALUES (?1, ?2, ?3, ?4, ?5, 0, ?6, 'mask')",
            params![
                session_id,
                segment_after_seq,
                requested.unwrap_or(call.seq),
                round.start_seq,
                round.cutoff_seq,
                crate::store::now_millis()
            ],
        )?;
        let checkpoint_id = tx.last_insert_rowid();
        tx.execute(
            "INSERT INTO memory_blocks
             (checkpoint_id, session_id, source_after_seq, cutoff_seq,
              summary, unmasked_order, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, NULL, ?6)",
            params![
                checkpoint_id,
                session_id,
                segment_after_seq,
                round.cutoff_seq,
                summary,
                crate::store::now_millis()
            ],
        )?;
        let generation = advance_generation(&tx, session_id)?;
        let view_generation = nonnegative(generation, "memory view generation")?;
        record_view_event(
            &tx,
            session_id,
            checkpoint_id,
            MASK,
            MEMORIZE_CAUSE,
            generation,
        )?;
        let start_seq = segment_after_seq
            .checked_add(1)
            .ok_or_else(|| anyhow!("memory source range overflow"))?;
        let output = serde_json::json!({
            "id": public_id(session_id, checkpoint_id),
            "source_range": format!("{start_seq}:{}", round.cutoff_seq),
            "view_generation": view_generation,
        })
        .to_string();
        let mut result = content.clone();
        result["output"] = Value::String(output.clone());
        let result_seq = Self::append_message_conn(&tx, session_id, Kind::ToolResult, &result)?;
        tx.execute(
            "UPDATE live_compact_checkpoints SET result_seq = ?2 WHERE id = ?1",
            params![checkpoint_id, result_seq],
        )?;
        artifacts.push(result_seq);
        for message_seq in artifacts {
            tx.execute(
                "INSERT INTO live_compact_artifacts (session_id, message_seq, checkpoint_id)
                 VALUES (?1, ?2, ?3)",
                params![session_id, message_seq, checkpoint_id],
            )?;
        }
        tx.execute(
            "UPDATE sessions SET live_compact_baseline_tokens = NULL,
                    live_compact_interval_effective = NULL
             WHERE id = ?1",
            params![session_id],
        )?;
        tx.commit()?;
        Ok(MemorizeCommit::Checkpointed(Memorized {
            id: public_id(session_id, checkpoint_id),
            output,
            checkpoint_id,
            source_after_seq: segment_after_seq,
            cutoff_seq: round.cutoff_seq,
            result_seq,
            view_generation,
        }))
    }

    /// Commits one recall, including FIFO displacement and its tool result, atomically.
    ///
    /// # Invariant
    ///
    /// Every id is validated before the view changes. Each recalled block moves to the tail
    /// of the unmasked queue, so a block read again is not the next one evicted; duplicate
    /// ids still have no additional effect beyond that one move.
    pub fn recall(
        &self,
        session_id: i64,
        call_id: &str,
        content: &Value,
        max_unmasked_tokens: Option<u64>,
    ) -> anyhow::Result<MemoryRecall> {
        let maximum_tokens = max_unmasked_tokens
            .map(|value| crate::memory::maximum(value))
            .transpose()?;
        let connection = self.picture_conn();
        let mut connection = connection.lock().unwrap_or_else(|error| error.into_inner());
        let tx = connection.transaction()?;
        let call = stored_call(&tx, session_id, call_id, crate::tools::memory::RECALL_TOOL)?;
        let ids = call
            .arguments
            .get("ids")
            .and_then(Value::as_array)
            .ok_or_else(|| anyhow!("recall requires an ids array"))?;
        if ids.is_empty() {
            bail!("recall requires at least one memory id");
        }
        let mut requested = Vec::new();
        let mut seen = std::collections::HashSet::new();
        for id in ids {
            let id = id
                .as_str()
                .ok_or_else(|| anyhow!("every recall id must be a string"))?;
            let checkpoint_id = checkpoint_from_public_id(session_id, id)?;
            if !seen.insert(checkpoint_id) {
                continue;
            }
            let (unmasked_order, row_tokens, retired) = tx
                .query_row(
                    "SELECT m.unmasked_order, m.token_count, EXISTS(
                         SELECT 1 FROM live_compact_checkpoints retired
                         WHERE retired.session_id = m.session_id AND retired.kind = 'cut'
                           AND retired.result_seq >= c.result_seq)
                     FROM memory_blocks m
                     JOIN live_compact_checkpoints c ON c.id = m.checkpoint_id
                     WHERE m.session_id = ?1 AND m.checkpoint_id = ?2",
                    params![session_id, checkpoint_id],
                    |row| {
                        Ok((
                            row.get::<_, Option<i64>>(0)?,
                            row.get::<_, Option<i64>>(1)?,
                            row.get::<_, bool>(2)?,
                        ))
                    },
                )
                .optional()?
                .ok_or_else(|| {
                    anyhow!("memory id {id:?} does not belong to session {session_id}")
                })?;
            if retired {
                bail!("memory id {id:?} was retired by a later cut checkpoint");
            }
            requested.push(RequestedBlock {
                checkpoint_id,
                id: id.to_string(),
                unmasked: unmasked_order.is_some(),
                tokens: row_tokens,
            });
        }

        ensure_state(&tx, session_id)?;
        let generation = advance_generation(&tx, session_id)?;
        let mut next_order = read_state(&tx, session_id)?.next_unmasked_order;
        let mut recalled = Vec::new();
        let mut recalled_tokens = 0_i64;
        for block in requested {
            let following_order = next_order.checked_add(1).ok_or_else(|| {
                anyhow!("memory unmasked order overflow for session {session_id}")
            })?;
            let changed = tx.execute(
                "UPDATE memory_blocks SET unmasked_order = ?3
                 WHERE session_id = ?1 AND checkpoint_id = ?2",
                params![session_id, block.checkpoint_id, next_order],
            )?;
            if changed != 1 {
                bail!("memory block {} changed while being recalled", block.id);
            }
            if !block.unmasked {
                record_view_event(
                    &tx,
                    session_id,
                    block.checkpoint_id,
                    UNMASK,
                    RECALL_CAUSE,
                    generation,
                )?;
                recalled_tokens = recalled_tokens.saturating_add(block.tokens.unwrap_or(0));
                recalled.push(block.id);
            }
            next_order = following_order;
        }
        tx.execute(
            "UPDATE memory_state SET next_unmasked_order = ?2 WHERE session_id = ?1",
            params![session_id, next_order],
        )?;
        let unmasked_tokens = if maximum_tokens.is_some() {
            projected_unmasked_tokens(&tx, session_id)?
                .map(|tokens| tokens.saturating_add(recalled_tokens))
        } else {
            None
        };
        let auto_masked = enforce_limit_at_generation(
            &tx,
            session_id,
            maximum_tokens,
            unmasked_tokens,
            RECALL_CAUSE,
            generation,
        )?;
        let view_generation = nonnegative(generation, "memory view generation")?;
        let output = serde_json::json!({
            "recalled": &recalled,
            "auto_masked": &auto_masked,
            "view_generation": view_generation,
        })
        .to_string();
        let mut result = content.clone();
        result["output"] = Value::String(output.clone());
        let result_seq = Self::append_message_conn(&tx, session_id, Kind::ToolResult, &result)?;
        tx.commit()?;
        Ok(MemoryRecall {
            output,
            result_seq,
            recalled,
            auto_masked,
            view_generation,
        })
    }

    /// Reads one immutable request snapshot; retries never ask for the current view again.
    pub fn memory_view(
        &self,
        session_id: i64,
        upto_seq: i64,
    ) -> anyhow::Result<crate::llm::MemoryView> {
        let connection = self.picture_conn();
        let connection = connection.lock().unwrap_or_else(|error| error.into_inner());
        let generation = connection
            .query_row(
                "SELECT view_generation FROM memory_state WHERE session_id = ?1",
                params![session_id],
                |row| row.get::<_, i64>(0),
            )
            .optional()?
            .unwrap_or(0);
        let mut statement = connection.prepare(
            "SELECT m.checkpoint_id, m.source_after_seq, m.cutoff_seq
             FROM memory_blocks m
             JOIN live_compact_checkpoints c ON c.id = m.checkpoint_id
             WHERE m.session_id = ?1 AND m.unmasked_order IS NULL
               AND c.result_seq <= ?2
               AND c.result_seq > COALESCE((
                   SELECT MAX(retired.result_seq) FROM live_compact_checkpoints retired
                   WHERE retired.session_id = m.session_id AND retired.kind = 'cut'
                     AND retired.result_seq <= ?2), 0)
             ORDER BY m.source_after_seq, m.checkpoint_id",
        )?;
        let rows = statement.query_map(params![session_id, upto_seq], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, i64>(2)?,
            ))
        })?;
        let mut masked = Vec::new();
        for row in rows {
            let (checkpoint_id, source_after_seq, cutoff_seq) = row?;
            let start_seq = source_after_seq
                .checked_add(1)
                .ok_or_else(|| anyhow!("memory source range overflow"))?;
            masked.push(crate::llm::MemoryMaskSpan {
                memory_id: public_id(session_id, checkpoint_id),
                start_seq,
                end_seq: cutoff_seq,
            });
        }
        Ok(crate::llm::MemoryView {
            upto_seq,
            view_generation: nonnegative(generation, "memory view generation")?,
            masked,
        })
    }

    /// Applies the visible-token limit without appending a tool result.
    ///
    /// # Invariant
    ///
    /// A view that already fits the budget changes neither visibility nor generation.
    pub fn enforce_limit(
        &self,
        session_id: i64,
        max_unmasked_tokens: Option<u64>,
        cause: &str,
    ) -> anyhow::Result<LimitEnforced> {
        let maximum_tokens = max_unmasked_tokens
            .map(|value| crate::memory::maximum(value))
            .transpose()?;
        let connection = self.picture_conn();
        let mut connection = connection.lock().unwrap_or_else(|error| error.into_inner());
        let tx = connection.transaction()?;
        ensure_state(&tx, session_id)?;
        let state = read_state(&tx, session_id)?;
        let unmasked_tokens = if maximum_tokens.is_some() {
            projected_unmasked_tokens(&tx, session_id)?
        } else {
            None
        };
        let over_tokens = state.unmasked_count > 0
            && maximum_tokens
                .is_some_and(|limit| unmasked_tokens.unwrap_or(0).saturating_sub(limit) > 0);
        if !over_tokens {
            return Ok(LimitEnforced {
                auto_masked: Vec::new(),
                view_generation: nonnegative(state.view_generation, "memory view generation")?,
            });
        }
        let generation = advance_generation(&tx, session_id)?;
        let auto_masked = enforce_limit_at_generation(
            &tx,
            session_id,
            maximum_tokens,
            unmasked_tokens,
            cause,
            generation,
        )?;
        let view_generation = nonnegative(generation, "memory view generation")?;
        tx.commit()?;
        Ok(LimitEnforced {
            auto_masked,
            view_generation,
        })
    }

    /// Clears tokenization-specific measurements when a session changes models.
    pub fn invalidate_memory_tokens(&self, session_id: i64) -> anyhow::Result<()> {
        let connection = self.picture_conn();
        let mut connection = connection.lock().unwrap_or_else(|error| error.into_inner());
        let tx = connection.transaction()?;
        tx.execute(
            "UPDATE memory_state SET unmasked_tokens = NULL WHERE session_id = ?1",
            params![session_id],
        )?;
        tx.execute(
            "UPDATE memory_blocks SET token_count = NULL WHERE session_id = ?1",
            params![session_id],
        )?;
        tx.commit()?;
        Ok(())
    }

    /// Persists provider-tokenized block sizes for later recall-budget arithmetic.
    pub fn note_memory_tokens(
        &self,
        session_id: i64,
        measurements: &[crate::llm::MemoryTokenUsage],
        unmasked_tokens: u64,
    ) -> anyhow::Result<()> {
        let connection = self.picture_conn();
        let mut connection = connection.lock().unwrap_or_else(|error| error.into_inner());
        let tx = connection.transaction()?;
        ensure_state(&tx, session_id)?;
        tx.execute(
            "UPDATE memory_state SET unmasked_tokens = ?2 WHERE session_id = ?1",
            params![session_id, maximum(unmasked_tokens)?],
        )?;
        for measurement in measurements {
            let checkpoint_id = checkpoint_from_public_id(session_id, &measurement.memory_id)?;
            let tokens = maximum(measurement.tokens)?;
            let changed = tx.execute(
                "UPDATE memory_blocks SET token_count = ?3
                 WHERE session_id = ?1 AND checkpoint_id = ?2",
                params![session_id, checkpoint_id, tokens],
            )?;
            if changed != 1 {
                bail!(
                    "provider reported memory id {:?}, which session {session_id} does not own",
                    measurement.memory_id
                );
            }
        }
        tx.commit()?;
        Ok(())
    }
}
