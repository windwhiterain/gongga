//! The transcript as a request reads it: one row at a time, and never all of it.
//!
//! See `docs/notes/server-store.md`.

use std::path::Path;

use rusqlite::{Connection, OpenFlags, params};

use crate::image::ImageRef;
use crate::store::images_of;

/// One sendable row.
pub struct Row {
    /// The stored Responses input item, byte for byte as the database holds it.
    pub item: String,
    /// Which entry this is, so a request pairs calls with results by name rather
    /// than by opening the item's bytes.
    pub kind: String,
    /// The tool a call row asks for, or a result row answers.
    pub call_id: Option<String>,
    /// Who spoke, for a text row: a prompt ends a step, an answer stays in it.
    pub role: Option<String>,
    /// The pictures this row's payload recorded, for a tool result that handed some
    /// back. Empty for every other row.
    pub images: Vec<ImageRef>,
}

/// A walk over one session's sendable rows in `(0, upto_seq]`, oldest first.
///
/// # Invariant
///
/// It holds its own connection rather than the store's: a request reads while it is
/// writing to the network, and the shared connection has to stay free for the turn
/// waiting on it. `upto_seq` is fixed when the walk is made, so every row was stored
/// before the request began -- which is what lets a retry replay one window.
pub struct Transcript {
    conn: Connection,
    session_id: i64,
    upto_seq: i64,
    after_seq: i64,
}

impl Transcript {
    /// Opens the database at `path` read-only and stands at its start.
    pub fn open(path: &Path, session_id: i64, upto_seq: i64) -> anyhow::Result<Self> {
        let conn = Connection::open_with_flags(
            path,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )?;
        Ok(Self {
            conn,
            session_id,
            upto_seq,
            after_seq: 0,
        })
    }
}

impl Iterator for Transcript {
    type Item = anyhow::Result<Row>;

    fn next(&mut self) -> Option<Self::Item> {
        // A keyset seek, one row per query: the index answers "the next sendable
        // row after this seq" in one descent.
        let found = self.conn.query_row(
            "SELECT seq, content, kind, display, call_id FROM messages
              WHERE session_id = ?1 AND seq > ?2 AND seq <= ?3 AND content IS NOT NULL
              ORDER BY seq LIMIT 1",
            params![self.session_id, self.after_seq, self.upto_seq],
            |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, String>(3)?,
                    r.get::<_, Option<String>>(4)?,
                ))
            },
        );
        let (seq, item, kind, display, call_id) = match found {
            Ok(row) => row,
            Err(rusqlite::Error::QueryReturnedNoRows) => return None,
            Err(e) => return Some(Err(e.into())),
        };
        self.after_seq = seq;
        let role = if kind == "text" {
            serde_json::from_str::<serde_json::Value>(&item)
                .ok()
                .and_then(|v| v.get("role").and_then(|r| r.as_str()).map(str::to_string))
        } else {
            None
        };
        let images = if kind == "tool_result" {
            let value = serde_json::from_str(&display).unwrap_or(serde_json::Value::Null);
            images_of(&value)
        } else {
            Vec::new()
        };
        Some(Ok(Row {
            item,
            kind,
            call_id,
            role,
            images,
        }))
    }
}
