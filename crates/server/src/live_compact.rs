//! Durable live-compaction state and the ordinary round a checkpoint retains.

use std::collections::BTreeMap;

use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};

pub const REQUEST_KIND: &str = "live_compact_request";

#[derive(Debug, Clone, Copy)]
pub struct Measurement {
    pub tokens: u64,
    pub baseline_tokens: Option<u64>,
    pub requested: bool,
    pub generation: u64,
}

#[derive(Debug, Clone, Copy)]
pub struct Round {
    pub start_seq: i64,
    pub cutoff_seq: i64,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct Request {
    pub request_seq: i64,
    pub segment_after_seq: i64,
    pub round_start_seq: i64,
    pub cutoff_seq: i64,
    pub generation: u64,
}

#[derive(Debug, Clone, Copy)]
pub struct Committed {
    pub checkpoint_id: i64,
    pub result_seq: i64,
    pub generation: u64,
}

/// Adds live compact to an existing home without inferring a baseline from old messages.
pub fn migrate(conn: &Connection) -> rusqlite::Result<()> {
    for (column, ddl) in [
        (
            "live_compact_origin_seq",
            "ALTER TABLE sessions ADD COLUMN live_compact_origin_seq INTEGER",
        ),
        (
            "live_compact_baseline_tokens",
            "ALTER TABLE sessions ADD COLUMN live_compact_baseline_tokens INTEGER",
        ),
        (
            "live_compact_requested",
            "ALTER TABLE sessions ADD COLUMN live_compact_requested INTEGER NOT NULL DEFAULT 0",
        ),
        (
            "live_compact_started",
            "ALTER TABLE sessions ADD COLUMN live_compact_started INTEGER NOT NULL DEFAULT 0",
        ),
    ] {
        if conn
            .prepare(&format!("SELECT {column} FROM sessions LIMIT 1"))
            .is_err()
        {
            conn.execute(ddl, [])?;
        }
    }
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS live_compact_checkpoints (
            id                INTEGER PRIMARY KEY,
            session_id        INTEGER NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
            segment_after_seq INTEGER NOT NULL,
            request_seq       INTEGER NOT NULL,
            round_start_seq   INTEGER NOT NULL,
            cutoff_seq        INTEGER NOT NULL,
            result_seq        INTEGER NOT NULL,
            created_at        INTEGER NOT NULL
        );
        CREATE INDEX IF NOT EXISTS live_compact_checkpoints_by_session
            ON live_compact_checkpoints(session_id, id);
        -- What a session sends, as a stored and ordered sequence: one row per
        -- item, keyed by the generation a request reads. A compaction commit writes
        -- the next generation and leaves the one before it exactly as it was.
        CREATE TABLE IF NOT EXISTS context_rows (
            session_id  INTEGER NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
            generation  INTEGER NOT NULL,
            position    INTEGER NOT NULL,
            message_seq INTEGER NOT NULL,
            -- 'input' is an element of the request's message array; 'field' is a
            -- provider field, named in `field`. The row says which; the request
            -- does not decide it.
            slot        TEXT NOT NULL DEFAULT 'input',
            field       TEXT,
            item        TEXT NOT NULL,
            pictures    TEXT NOT NULL DEFAULT '[]',
            PRIMARY KEY (session_id, generation, position)
        );
        CREATE TABLE IF NOT EXISTS live_compact_artifacts (
            session_id   INTEGER NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
            message_seq  INTEGER NOT NULL,
            checkpoint_id INTEGER NOT NULL REFERENCES live_compact_checkpoints(id) ON DELETE CASCADE,
            PRIMARY KEY (session_id, message_seq)
        );",
    )?;
    // After the table it widens: a home written before the slot existed gets it here.
    for (column, ddl) in [
        (
            "slot",
            "ALTER TABLE context_rows ADD COLUMN slot TEXT NOT NULL DEFAULT 'input'",
        ),
        ("field", "ALTER TABLE context_rows ADD COLUMN field TEXT"),
    ] {
        if conn
            .prepare(&format!("SELECT {column} FROM context_rows LIMIT 1"))
            .is_err()
        {
            conn.execute(ddl, [])?;
        }
    }
    Ok(())
}

/// Finds a complete ordinary round; malformed or interrupted rounds suppress compaction.
pub fn ordinary_round(
    conn: &Connection,
    session_id: i64,
    segment_after_seq: i64,
) -> anyhow::Result<Option<Round>> {
    let latest = conn
        .query_row(
            "SELECT m.seq, m.display FROM messages m
             JOIN tool_calls t ON t.session_id = m.session_id AND t.call_id = m.call_id
             WHERE m.session_id = ?1 AND m.kind = 'tool_result'
               AND t.name <> 'live_compact' AND m.seq > ?2
             ORDER BY m.seq DESC LIMIT 1",
            params![session_id, segment_after_seq],
            |row| Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?)),
        )
        .optional()?;
    let Some((cutoff_seq, display)) = latest else {
        return Ok(None);
    };
    let value: serde_json::Value = serde_json::from_str(&display)?;
    let Some(step) = value.get("step").and_then(serde_json::Value::as_u64) else {
        return Ok(None);
    };

    let mut statement = conn.prepare(
        "SELECT seq, kind, call_id, display FROM messages
         WHERE session_id = ?1 AND seq > ?2 AND seq <= ?3
         ORDER BY seq DESC",
    )?;
    let rows = statement.query_map(params![session_id, segment_after_seq, cutoff_seq], |row| {
        Ok((
            row.get::<_, i64>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, Option<String>>(2)?,
            row.get::<_, String>(3)?,
        ))
    })?;
    let mut entries = Vec::new();
    for row in rows {
        let (seq, kind, call_id, display) = row?;
        let value: serde_json::Value = serde_json::from_str(&display)?;
        let same_step = value.get("step").and_then(serde_json::Value::as_u64) == Some(step);
        if !same_step || (kind != "tool_call" && kind != "tool_result") {
            break;
        }
        if kind == "tool_call"
            && value.get("name").and_then(serde_json::Value::as_str)
                == Some(crate::tools::live_compact::LIVE_COMPACT_TOOL)
        {
            return Ok(None);
        }
        entries.push((seq, kind, call_id));
    }
    let Some(start_seq) = entries.last().map(|(seq, _, _)| *seq) else {
        return Ok(None);
    };

    let mut pairs: BTreeMap<String, (bool, bool)> = BTreeMap::new();
    for (_, kind, call_id) in &entries {
        let Some(call_id) = call_id.clone().filter(|id| !id.is_empty()) else {
            return Ok(None);
        };
        let pair = pairs.entry(call_id).or_default();
        let side = if kind == "tool_call" {
            &mut pair.0
        } else {
            &mut pair.1
        };
        if *side {
            return Ok(None);
        }
        *side = true;
    }
    if pairs.is_empty() || pairs.values().any(|(call, result)| !call || !result) {
        return Ok(None);
    }

    // Contiguous pre-call reasoning is retained; an earlier item ends that association.
    let mut round_start_seq = start_seq;
    let mut statement = conn.prepare(
        "SELECT seq, kind FROM messages
         WHERE session_id = ?1 AND seq > ?2 AND seq < ?3
         ORDER BY seq DESC",
    )?;
    let rows = statement.query_map(params![session_id, segment_after_seq, start_seq], |row| {
        Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
    })?;
    for row in rows {
        let (seq, kind) = row?;
        if kind != "reasoning" {
            break;
        }
        round_start_seq = seq;
    }
    Ok(Some(Round {
        start_seq: round_start_seq,
        cutoff_seq,
    }))
}
