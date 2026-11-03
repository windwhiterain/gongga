//! The transcript as a request reads it: one row at a time, and never all of it.
//!
//! See `docs/notes/server-store.md`.

use std::path::Path;

use rusqlite::{Connection, OpenFlags, params};

/// A walk over one session's sendable rows in `(0, upto_seq]`, oldest first.
///
/// # Invariant
///
/// What comes back is the stored item, byte for byte: the request's shape was
/// decided when the row was appended, so a walk cannot disagree with an earlier
/// request about what a row is. It holds its own connection rather than the
/// store's: a request reads while it is writing to the network, and the shared
/// connection has to stay free for the turn waiting on it. `upto_seq` is fixed
/// when the walk is made, so every row was stored before the request began --
/// which is what lets a retry replay one window.
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
    type Item = anyhow::Result<crate::body::Stored>;

    fn next(&mut self) -> Option<Self::Item> {
        // A keyset seek, one row per query: the index answers "the next sendable
        // row after this seq" in one descent.
        let found = self.conn.query_row(
            "SELECT seq, content, kind, display FROM messages
              WHERE session_id = ?1 AND seq > ?2 AND seq <= ?3 AND content IS NOT NULL
              ORDER BY seq LIMIT 1",
            params![self.session_id, self.after_seq, self.upto_seq],
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
        // The pictures ride in `display`, which is what the row recorded when it was
        // written: an item names a copy, and the copy is read when a request is built.
        let pictures = match kind.as_str() {
            "tool_result" | "text" => {
                let value = serde_json::from_str(&display).unwrap_or_default();
                crate::store::images_of(&value)
            }
            _ => Vec::new(),
        };
        Some(Ok(crate::body::Stored { item, pictures }))
    }
}
