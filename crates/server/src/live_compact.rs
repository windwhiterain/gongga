//! Durable live-compaction state: the checkpoint and the interval that asks for the next one.

use std::collections::BTreeMap;

use rusqlite::{Connection, OptionalExtension, params};

pub const REQUEST_KIND: &str = "live_compact_request";

/// What a committed checkpoint does to the canonical transcript's provider projection.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CompactMode {
    #[default]
    Cut,
    Mask,
}

impl CompactMode {
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "cut" => Some(Self::Cut),
            "mask" => Some(Self::Mask),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Cut => "cut",
            Self::Mask => "mask",
        }
    }
}

/// What one step measures: the context, the last checkpoint, and the backoff.
#[derive(Debug, Clone, Copy)]
pub struct Measurement {
    pub tokens: u64,
    pub baseline_tokens: Option<u64>,
    /// The interval in force after every unanswered ask; `None` is the base.
    pub interval: Option<u64>,
    pub generation: u64,
}

#[derive(Debug, Clone, Copy)]
pub struct Round {
    pub start_seq: i64,
    pub cutoff_seq: i64,
}

#[derive(Debug, Clone, Copy)]
pub struct Committed {
    pub checkpoint_id: i64,
    pub result_seq: i64,
    pub generation: u64,
    /// Whether the turn goes on after this step; a `continue=false` call ends it there.
    pub continue_turn: bool,
}

/// What a `live_compact` call did: committed a checkpoint, or explained why not.
///
/// # Invariant
///
/// "Nothing to compact" is an answer, not a failure: an error invites a retry loop,
/// and the head that loop re-sends is paid for every time. See
/// `docs/notes/server-live-compact.md`.
#[derive(Debug, Clone)]
pub enum Commit {
    Checkpointed(Committed),
    Nothing(String),
}

/// Where the segment a checkpoint folds begins, and where the last one ended.
#[derive(Debug, Clone, Copy, Default)]
pub struct Bounds {
    /// The last checkpoint's cutoff, or the origin: the first row a summary replaces.
    pub segment_after: Option<i64>,
    /// The last checkpoint's result row, so a request older than it is spent.
    pub last_result_seq: Option<i64>,
}

/// Reads both boundaries in one look, so the delivery and the commit cannot disagree.
pub fn bounds(conn: &Connection, session_id: i64) -> rusqlite::Result<Bounds> {
    let origin = conn
        .query_row(
            "SELECT live_compact_origin_seq FROM sessions WHERE id = ?1",
            params![session_id],
            |row| row.get::<_, Option<i64>>(0),
        )
        .optional()?
        .flatten();
    let last = conn
        .query_row(
            "SELECT cutoff_seq, result_seq FROM live_compact_checkpoints
              WHERE session_id = ?1 ORDER BY id DESC LIMIT 1",
            params![session_id],
            |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)),
        )
        .optional()?;
    Ok(Bounds {
        segment_after: last.map(|(cutoff, _)| cutoff).or(origin),
        last_result_seq: last.map(|(_, result)| result),
    })
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
            "live_compact_interval_effective",
            "ALTER TABLE sessions ADD COLUMN live_compact_interval_effective INTEGER",
        ),
    ] {
        if conn
            .prepare(&format!("SELECT {column} FROM sessions LIMIT 1"))
            .is_err()
        {
            conn.execute(ddl, [])?;
        }
    }
    // The flags the backoff replaced: a column nothing reads is a lie about the schema.
    for column in ["live_compact_requested", "live_compact_started"] {
        if conn
            .prepare(&format!("SELECT {column} FROM sessions LIMIT 1"))
            .is_ok()
        {
            conn.execute(&format!("ALTER TABLE sessions DROP COLUMN {column}"), [])?;
        }
    }
    conn.execute(
        "DELETE FROM pending_tasks
          WHERE kind = 'inject'
            AND json_extract(payload, '$.live_compact') IS NOT NULL",
        [],
    )?;
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
        CREATE TABLE IF NOT EXISTS live_compact_artifacts (
            session_id   INTEGER NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
            message_seq  INTEGER NOT NULL,
            checkpoint_id INTEGER NOT NULL REFERENCES live_compact_checkpoints(id) ON DELETE CASCADE,
            PRIMARY KEY (session_id, message_seq)
        );",
    )
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
            && value
                .get("name")
                .and_then(serde_json::Value::as_str)
                .is_some_and(|name| {
                    [
                        crate::tools::live_compact::LIVE_COMPACT_TOOL,
                        crate::tools::memory::MEMORIZE_TOOL,
                        crate::tools::memory::RECALL_TOOL,
                    ]
                    .contains(&name)
                })
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
