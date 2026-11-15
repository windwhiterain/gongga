//! The transcript as a request reads it: one row at a time, and never all of it.
//!
//! See `docs/notes/server-store.md`.

use std::path::Path;

use rusqlite::{Connection, OpenFlags, OptionalExtension, params};

/// A walk over one session's sendable rows in `(0, upto_seq]`, oldest first.
///
/// # Invariant
///
/// What comes back is the stored item, byte for byte: the request's shape was
/// decided when the row was appended, so a walk cannot disagree with an earlier
/// request about what a row is.
///
/// It holds its own connection rather than the store's, and `upto_seq` is fixed
/// when the walk is made. See `docs/notes/server-transcript.md`.
pub struct Transcript {
    conn: Connection,
    session_id: i64,
    upto_seq: i64,
    after_seq: i64,
    /// Permanent-prefix end and latest ordinary-round start, once compacted.
    projection: Option<(i64, i64)>,
}

impl Transcript {
    /// Opens the database at `path` read-only and stands at its start.
    pub fn open(path: &Path, session_id: i64, upto_seq: i64) -> anyhow::Result<Self> {
        let conn = Connection::open_with_flags(
            path,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )?;
        let latest = conn
            .query_row(
                "SELECT s.live_compact_origin_seq, c.round_start_seq
                 FROM live_compact_checkpoints c
                 JOIN sessions s ON s.id = c.session_id
                 WHERE c.session_id = ?1 AND c.result_seq <= ?2
                 ORDER BY c.id DESC LIMIT 1",
                params![session_id, upto_seq],
                |row| Ok((row.get::<_, Option<i64>>(0)?, row.get::<_, i64>(1)?)),
            )
            .optional()?;
        let projection = match latest
            .and_then(|(origin, round_start)| origin.map(|origin| (origin, round_start)))
        {
            Some((origin, round_start)) => {
                Some((whole_step_end(&conn, session_id, origin)?, round_start))
            }
            None => None,
        };
        Ok(Self {
            conn,
            session_id,
            upto_seq,
            after_seq: 0,
            projection,
        })
    }
}

/// Where a permanent prefix may end: `origin` moved back off a call whose output it
/// would not hold.
///
/// # Invariant
///
/// A `tool_call` never ends a prefix: its output is the row after it, so a cut there
/// withholds half a pair and the provider refuses that request. The call that opens a
/// checkpoint is the agent's own, so a checkpoint written before this rule names it,
/// and moving the cut back is what lets it be read. See
/// `docs/notes/server-live-compact.md`.
fn whole_step_end(conn: &Connection, session_id: i64, origin: i64) -> anyhow::Result<i64> {
    let end = conn.query_row(
        "SELECT COALESCE(MAX(seq), 0) FROM messages
          WHERE session_id = ?1 AND seq <= ?2 AND kind <> 'tool_call'",
        params![session_id, origin],
        |row| row.get(0),
    )?;
    Ok(end)
}

impl Iterator for Transcript {
    type Item = anyhow::Result<crate::body::Stored>;

    fn next(&mut self) -> Option<Self::Item> {
        // Indexed artifacts, prefix, and latest round plus later rows; raw rows stay unchanged.
        let found = self.conn.query_row(
            "SELECT seq, content, kind, display FROM messages
              WHERE session_id = ?1 AND seq > ?2 AND seq <= ?3 AND content IS NOT NULL
                AND (?4 = 0 OR seq <= ?5 OR seq >= ?6
                     OR EXISTS (SELECT 1 FROM live_compact_artifacts a
                                WHERE a.session_id = messages.session_id
                                  AND a.message_seq = messages.seq))
              ORDER BY seq LIMIT 1",
            params![
                self.session_id,
                self.after_seq,
                self.upto_seq,
                self.projection.is_some(),
                self.projection.map(|p| p.0).unwrap_or(0),
                self.projection.map(|p| p.1).unwrap_or(0),
            ],
            |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, Option<String>>(1)?.unwrap_or_default(),
                    r.get::<_, String>(2)?,
                    r.get::<_, String>(3)?,
                ))
            },
        );
        let (seq, item, kind, display) = match found {
            Ok(row) => row,
            Err(rusqlite::Error::QueryReturnedNoRows) => return None,
            Err(e) => return Some(Err(e.into())),
        };
        self.after_seq = seq;
        // The pictures ride in `display`, what the row recorded: a copy is read
        // when a request is built.
        let pictures = match crate::store::Kind::parse(&kind) {
            Some(
                crate::store::Kind::Text
                | crate::store::Kind::Inject
                | crate::store::Kind::ToolResult,
            ) => {
                let value = serde_json::from_str(&display).unwrap_or_default();
                crate::store::images_of(&value)
            }
            Some(
                crate::store::Kind::Reasoning
                | crate::store::Kind::ToolCall
                | crate::store::Kind::Notice
                | crate::store::Kind::ThinkingMark
                | crate::store::Kind::System
                | crate::store::Kind::Tools
                | crate::store::Kind::BuiltinCall
                | crate::store::Kind::LiveCompactRequest,
            ) => Vec::new(),
            None => Vec::new(),
        };
        Some(Ok(crate::body::Stored { item, pictures }))
    }
}
