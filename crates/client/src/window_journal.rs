//! The window's own facts, held by the client rather than a front-end.
//!
//! See `docs/notes/client-window-journal.md`.

use std::collections::{BTreeMap, BTreeSet};

use crate::render_wire::CursorMessage;

/// What the client knows about the windows it has drawn.
///
/// # Invariant
///
/// No terminal type and no query crosses a process boundary: a frame reports its
/// facts, the cursor half of `Sink` is consumed here, and a refill reads the
/// answer from this one place. See `docs/notes/client-window-journal.md`.
#[derive(Debug, Default)]
pub struct WindowJournal {
    stamps: BTreeMap<String, (i64, i64)>,
    floors: BTreeMap<String, i64>,
    drawn: Vec<String>,
    room: BTreeMap<String, usize>,
    wants: BTreeSet<String>,
    settled: Vec<(u64, bool)>,
}

impl WindowJournal {
    pub fn new() -> Self {
        Self::default()
    }

    /// Eats the cursor half of `Sink`: those answers belong to the client.
    ///
    /// # Invariant
    ///
    /// `Rewind` keeps the stamp, because everything up to it came from rows that
    /// still exist; `Older` does not move it, because a page of the past lands
    /// above the held rows. `Landed` names both the new stamp and the new floor.
    pub fn consume(&mut self, cursor: CursorMessage) {
        match cursor {
            CursorMessage::Settled { id, ok } => self.settled.push((id, ok)),
            CursorMessage::Transcript {
                session,
                created,
                seq,
            } => {
                self.stamps.insert(session, (created, seq));
            }
            CursorMessage::Landed {
                window,
                created,
                drawn,
                floor,
            } => {
                self.stamps.insert(window.clone(), (created, drawn));
                self.floors.insert(window, floor);
            }
            CursorMessage::Rewind { .. } | CursorMessage::Older { .. } => {}
        }
    }

    /// What a window has drawn, and of which session.
    pub fn stamp_of(&self, window: &str) -> Option<(i64, i64)> {
        self.stamps.get(window).copied()
    }

    /// The oldest transcript row a window holds.
    pub fn floor_of(&self, window: &str) -> Option<i64> {
        self.floors.get(window).copied()
    }

    /// The windows the last frame drew.
    pub fn drawn(&self) -> &[String] {
        &self.drawn
    }

    /// The rows a window's pane could draw last frame: what one page is sized by.
    ///
    /// # Invariant
    ///
    /// A page is what a pane can show, never a constant, and a pane that has not
    /// reported yet still asks for one row. See `docs/notes/tui-cache.md`.
    pub fn room_of(&self, window: &str) -> usize {
        self.room.get(window).copied().unwrap_or(1)
    }

    /// The commands the server answered since the last frame.
    pub fn take_settled(&mut self) -> Vec<(u64, bool)> {
        std::mem::take(&mut self.settled)
    }

    /// Starts a frame: nothing has been drawn yet.
    pub fn begin_frame(&mut self) {
        self.drawn.clear();
    }

    /// Says this window's rows were drawn this frame.
    pub fn mark_drawn(&mut self, window: &str) {
        if !self.drawn.iter().any(|name| name == window) {
            self.drawn.push(window.to_string());
        }
    }

    /// Records how many rows a pane could draw this frame.
    pub fn note_room(&mut self, window: &str, rows: usize) {
        self.room.insert(window.to_string(), rows.max(1));
    }

    /// Records that the operator scrolled past the oldest row this window holds.
    ///
    /// # Invariant
    ///
    /// A keystroke, never a frame: the page above is owed because someone asked
    /// to read it, so a window sitting at its oldest row is not refilled again.
    pub fn want(&mut self, window: &str) {
        self.wants.insert(window.to_string());
    }

    /// Takes the windows owed a page above what they hold.
    pub fn take_wants(&mut self) -> Vec<String> {
        std::mem::take(&mut self.wants).into_iter().collect()
    }

    /// Forgets a window whose session was deleted.
    pub fn forget(&mut self, window: &str) {
        self.stamps.remove(window);
        self.floors.remove(window);
        self.room.remove(window);
        self.wants.remove(window);
        self.drawn.retain(|name| name != window);
    }
}
