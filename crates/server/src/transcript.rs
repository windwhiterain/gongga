//! The transcript as a request reads it: the stored sequence, in order.
//!
//! See `docs/notes/server-context.md`.

use std::path::Path;

use rusqlite::{Connection, OpenFlags, params};

/// A walk over one session's stored sequence, oldest position first.
///
/// # Invariant
///
/// It selects and sends the stored sequence, and decides nothing: no row is
/// dropped or substituted here. `upto_seq` is fixed when the walk is made, which
/// is what a retry replays. See `docs/notes/server-context.md`.
pub struct Transcript {
    conn: Connection,
    session_id: i64,
    upto_seq: i64,
    position: i64,
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
            position: 0,
        })
    }
}

impl Iterator for Transcript {
    type Item = anyhow::Result<crate::body::Stored>;

    fn next(&mut self) -> Option<Self::Item> {
        // The sequence the session is sending now, in the order it was written.
        let found = self.conn.query_row(
            "SELECT r.position, r.item, r.pictures
             FROM context_rows r
             WHERE r.session_id = ?1 AND r.position > ?2 AND r.message_seq <= ?3
               AND r.generation = (SELECT COALESCE(MAX(generation), 0) FROM context_rows
                                    WHERE session_id = ?1)
             ORDER BY r.position LIMIT 1",
            params![self.session_id, self.position, self.upto_seq],
            |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                ))
            },
        );
        let (position, item, pictures) = match found {
            Ok(row) => row,
            Err(rusqlite::Error::QueryReturnedNoRows) => return None,
            Err(e) => return Some(Err(e.into())),
        };
        self.position = position;
        // The pictures ride with the item: a copy is read when a request is built.
        let pictures: Vec<crate::image::ImageRef> =
            serde_json::from_str::<serde_json::Value>(&pictures)
                .ok()
                .and_then(|value| {
                    value.as_array().map(|list| {
                        list.iter()
                            .filter_map(crate::image::ImageRef::from_json)
                            .collect()
                    })
                })
                .unwrap_or_default();
        Some(Ok(crate::body::Stored { item, pictures }))
    }
}
