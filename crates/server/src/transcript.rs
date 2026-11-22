//! The transcript as a request reads it: one row at a time, and never all of it.
//!
//! See `docs/notes/server-store.md`.

use std::collections::VecDeque;
use std::path::Path;

use rusqlite::{Connection, OpenFlags, OptionalExtension, params};

use crate::store::Kind;

/// A walk over one session's sendable rows in `(0, upto_seq]`, oldest first.
///
/// # Invariant
///
/// What comes back is the stored item, byte for byte, except for order inside a step: a
/// step's calls come before the outputs they answered, because a request carries one
/// assistant turn. It holds its own connection, and `upto_seq` is fixed when the walk is
/// made. See `docs/notes/server-transcript.md`.
pub struct Transcript {
    conn: Connection,
    session_id: i64,
    upto_seq: i64,
    after_seq: i64,
    /// Permanent-prefix end and latest ordinary-round start, once compacted.
    projection: Option<(i64, i64)>,
    /// A row read ahead of the output held in front of it.
    pending: VecDeque<Row>,
    /// The step being collected: its outputs, in the order they arrived.
    held: VecDeque<Row>,
    /// Outputs whose step is over, ready to go out before anything read after them.
    released: VecDeque<Row>,
    /// The `(turn, step)` the held outputs belong to.
    held_step: Option<(i64, i64)>,
}

/// One row as it was read, before the walk decides where it goes.
struct Row {
    seq: i64,
    stored: crate::body::Stored,
    kind: Option<crate::store::Kind>,
    step: Option<(i64, i64)>,
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
                 WHERE c.session_id = ?1 AND c.result_seq <= ?2 AND c.kind = 'cut'
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
            pending: VecDeque::new(),
            held: VecDeque::new(),
            released: VecDeque::new(),
            held_step: None,
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
        loop {
            if let Some(released) = self.released.pop_front() {
                return Some(Ok(released.stored));
            }
            let row = match self.pending.pop_front() {
                Some(row) => row,
                None => match self.read_one() {
                    Ok(Some(row)) => row,
                    Ok(None) => {
                        // The walk is over: whatever the last step still held goes out.
                        self.release(None);
                        return self.released.pop_front().map(|row| Ok(row.stored));
                    }
                    Err(e) => return Some(Err(e)),
                },
            };
            let Some(step) = self.held_step else {
                // No step is being collected: an output waits for the call its step
                // still has to make, so the step's calls can go out together.
                if matches!(row.kind, Some(Kind::ToolResult))
                    && let Some(step) = row.step
                {
                    match self.call_after(row.seq, step) {
                        Ok(true) => {
                            self.held_step = Some(step);
                            self.held.push_back(row);
                            continue;
                        }
                        Ok(false) => {}
                        Err(e) => return Some(Err(e)),
                    }
                }
                return Some(Ok(row.stored));
            };
            // A step is being collected: its calls go out as they arrive, and its
            // outputs wait until the step has no call left to make.
            if matches!(row.kind, Some(Kind::ToolCall)) && row.step == Some(step) {
                return Some(Ok(row.stored));
            }
            if !(matches!(row.kind, Some(Kind::ToolResult)) && row.step == Some(step)) {
                self.release(Some(row));
                continue;
            }
            let more = match self.call_after(row.seq, step) {
                Ok(more) => more,
                Err(e) => return Some(Err(e)),
            };
            self.held.push_back(row);
            if !more {
                self.release(None);
            }
        }
    }
}

impl Transcript {
    /// Sends the held outputs, and `row` after them, before anything the walk reads
    /// next: the step they answer is over.
    ///
    /// # Invariant
    ///
    /// Every output of a step goes out before any row that follows the step, so the
    /// step's calls are all behind a reader that pairs a tool message with the call
    /// before it. Releasing one at a time puts the next call in front of the outputs
    /// it answers, which a provider reading by position refuses.
    fn release(&mut self, row: Option<Row>) {
        self.held_step = None;
        self.released.extend(self.held.drain(..));
        self.released.extend(row);
    }
}

impl Transcript {
    /// The next row this walk may send, exactly as it was stored.
    fn read_one(&mut self) -> anyhow::Result<Option<Row>> {
        // Indexed artifacts, prefix, and latest round plus later rows; raw rows stay unchanged.
        let found = self.conn.query_row(
            "SELECT seq, content, kind, display, turn, step FROM messages
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
                    r.get::<_, Option<i64>>(4)?,
                    r.get::<_, Option<i64>>(5)?,
                ))
            },
        );
        let (seq, item, kind, display, turn, step) = match found {
            Ok(row) => row,
            Err(rusqlite::Error::QueryReturnedNoRows) => return Ok(None),
            Err(e) => return Err(e.into()),
        };
        self.after_seq = seq;
        // The pictures ride in `display`, what the row recorded: a copy is read
        // when a request is built.
        let pictures = match Kind::parse(&kind) {
            Some(Kind::Text | Kind::Inject | Kind::ToolResult) => {
                let value = serde_json::from_str(&display).unwrap_or_default();
                crate::store::images_of(&value)
            }
            Some(
                Kind::Reasoning
                | Kind::Error
                | Kind::Notice
                | Kind::ThinkingMark
                | Kind::System
                | Kind::Tools
                | Kind::Instructions
                | Kind::AgentsMd
                | Kind::IndexRow
                | Kind::Info
                | Kind::ToolCall
                | Kind::BuiltinCall
                | Kind::LiveCompactRequest,
            ) => Vec::new(),
            None => Vec::new(),
        };
        Ok(Some(Row {
            seq,
            stored: crate::body::Stored {
                seq,
                item,
                pictures,
            },
            kind: Kind::parse(&kind),
            step: turn.zip(step),
        }))
    }

    /// Whether `step` still has a call to make after `seq`: what an output at `seq`
    /// is waiting for.
    fn call_after(&self, seq: i64, step: (i64, i64)) -> anyhow::Result<bool> {
        let found = self.conn.query_row(
            "SELECT EXISTS (SELECT 1 FROM messages
                             WHERE session_id = ?1 AND turn = ?2 AND step = ?3
                               AND kind = 'tool_call' AND seq > ?4)",
            params![self.session_id, step.0, step.1, seq],
            |row| row.get::<_, bool>(0),
        )?;
        Ok(found)
    }
}
