//! The window's own facts, held by the client rather than a front-end.
//!
//! See `docs/notes/client-window-journal.md`.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use crate::protocol;
use crate::render_wire::CursorMessage;
use crate::window_cache::{Cache, Record};

/// What the client knows about the windows it has drawn, and their journals.
///
/// # Invariant
///
/// No terminal type and no query crosses a process boundary: a frame reports its
/// facts, the cursor half of `Sink` is consumed here, and a refill reads the
/// answer from this one place. The disk journal is owned here too, so a stamp
/// survives the process that wrote it. See `docs/notes/client-window-journal.md`.
pub struct WindowJournal {
    stamps: BTreeMap<String, (i64, i64)>,
    floors: BTreeMap<String, i64>,
    drawn: Vec<String>,
    room: BTreeMap<String, usize>,
    wants: BTreeSet<String>,
    settled: Vec<(u64, bool)>,
    /// One journal per window, under this client's own root and endpoint.
    cache: Cache,
}

impl Default for WindowJournal {
    fn default() -> Self {
        Self {
            stamps: BTreeMap::new(),
            floors: BTreeMap::new(),
            drawn: Vec::new(),
            room: BTreeMap::new(),
            wants: BTreeSet::new(),
            settled: Vec::new(),
            // A log built with no front-end in mind invents no directory.
            cache: Cache::none(),
        }
    }
}

impl WindowJournal {
    pub fn new() -> Self {
        Self::default()
    }

    /// The facts and the journals of one endpoint under this client's root.
    pub fn with_cache(root: &Path, endpoint: &str) -> Self {
        Self {
            cache: Cache::new(root, endpoint),
            ..Self::default()
        }
    }

    /// Points at another endpoint's journals: `connect_server` moved the address.
    ///
    /// # Invariant
    ///
    /// Another endpoint is another journal, so the in-memory stamps and floors
    /// must not answer for it. Frame facts stay: what a frame drew is still what
    /// it drew, and `drawn` is rebuilt by the next frame anyway.
    pub fn repoint(&mut self, root: &Path, endpoint: &str) {
        self.cache = Cache::new(root, endpoint);
        self.stamps.clear();
        self.floors.clear();
    }

    /// The journals themselves, for a view that reads blocks back off disk.
    pub fn cache(&self) -> &Cache {
        &self.cache
    }

    /// Eats the cursor half of `Sink`: those answers belong to the client.
    ///
    /// # Invariant
    ///
    /// `Rewind` keeps the stamp, because everything up to it came from rows that
    /// still exist; `Older` does not move it, because a page of the past lands
    /// above the held rows. A stamp or a floor is written down, so the next
    /// process answers with the same number. See `docs/notes/client-window-journal.md`.
    pub fn consume(&mut self, cursor: CursorMessage) {
        match cursor {
            CursorMessage::Settled { id, ok } => self.settled.push((id, ok)),
            CursorMessage::Transcript {
                session,
                created,
                seq,
            } => {
                self.cache.stamp(&session, created, seq);
                self.stamps.insert(session, (created, seq));
            }
            CursorMessage::Landed {
                window,
                created,
                drawn,
                floor,
            } => {
                let (floor, drawn) = match self.stamp_of(&window).zip(self.floor_of(&window)) {
                    Some(((old_created, old_drawn), old_floor))
                        if old_created == created
                            && floor <= old_drawn.saturating_add(1)
                            && drawn.saturating_add(1) >= old_floor =>
                    {
                        (floor.min(old_floor), drawn.max(old_drawn))
                    }
                    _ => (floor, drawn),
                };
                self.cache.append(&window, &Record::Floor { seq: floor });
                self.cache.stamp(&window, created, drawn);
                self.stamps.insert(window.clone(), (created, drawn));
                self.floors.insert(window, floor);
            }
            CursorMessage::Rewind { window } => {
                let _ = self.rewind(&window);
            }
            CursorMessage::Older { .. } => {}
        }
    }

    /// Records that every row up to `seq` of the session is on screen.
    pub fn stamp(&mut self, window: &str, created: i64, seq: i64) {
        self.cache.stamp(window, created, seq);
        self.stamps.insert(window.to_string(), (created, seq));
    }

    /// Writes down one row of a transcript: the client's copy of the server's state.
    ///
    /// # Invariant
    ///
    /// Written where the row is drawn, with the number the server gave it, so the
    /// journal and the window say the same thing about the same row. This is the
    /// client's and not a front-end's: only the side that receives a `(seq, event)`
    /// pair can key a row by its number. See `docs/notes/client-window-journal.md`.
    pub fn event(&mut self, window: &str, seq: u64, session: &str, body: &protocol::Event) {
        self.cache.append(
            window,
            &Record::Event {
                seq,
                session: session.to_string(),
                body: body.clone(),
            },
        );
    }

    /// The rows this window holds above `before`, newest first, at most `count`.
    pub fn events(&self, window: &str, before: u64, count: usize) -> Vec<(u64, protocol::Event)> {
        self.cache.events(window, before, count)
    }

    /// What a window has drawn, and of which session: the disk is the authority.
    ///
    /// # Invariant
    ///
    /// The journal outlives the process, so a fresh client answers with the same
    /// number; the in-memory map is only for a journal that keeps nothing on disk.
    pub fn stamp_of(&self, window: &str) -> Option<(i64, i64)> {
        let end = self.cache.end(window);
        self.cache
            .stamp_at(window, end)
            .map(|(_, created, seq)| (created, seq))
            .or_else(|| self.stamps.get(window).copied())
    }

    /// The oldest transcript row a window holds.
    pub fn floor_of(&self, window: &str) -> Option<i64> {
        self.floors.get(window).copied().or_else(|| {
            let end = self.cache.end(window);
            self.cache.floor(window, end).map(|(_, seq)| seq)
        })
    }

    /// Throws away what no stored row accounts for, and answers where to cut.
    ///
    /// # Invariant
    ///
    /// The cut is a position in the journal, not a count of blocks: every block
    /// above the newest stamp was drawn after the last row the stamp accounts for.
    pub fn rewind(&mut self, window: &str) -> Option<u64> {
        self.cache.rewind(window);
        self.stamp_offset(window)
    }

    /// Where the newest stamp of a window sits in its journal, if it has one.
    pub fn stamp_offset(&self, window: &str) -> Option<u64> {
        let end = self.cache.end(window);
        self.cache.stamp_at(window, end).map(|(at, _, _)| at)
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
        self.cache.remove(window);
        self.stamps.remove(window);
        self.floors.remove(window);
        self.wants.remove(window);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_stamp_outlives_the_journal_that_wrote_it() {
        let at = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!("gongga-journal-{}-{at}", std::process::id()));
        {
            let mut journal = WindowJournal::with_cache(&root, "test-endpoint");
            assert_eq!(journal.stamp_of("cmd"), None);
            journal.consume(CursorMessage::Transcript {
                session: "cmd".to_string(),
                created: 42,
                seq: 7,
            });
            assert_eq!(journal.stamp_of("cmd"), Some((42, 7)));
        }
        {
            let mut fresh = WindowJournal::with_cache(&root, "test-endpoint");
            assert_eq!(fresh.stamp_of("cmd"), Some((42, 7)));
            fresh.consume(CursorMessage::Landed {
                window: "cmd".to_string(),
                created: 42,
                drawn: 9,
                floor: 3,
            });
            assert_eq!(fresh.stamp_of("cmd"), Some((42, 9)));
            assert_eq!(fresh.floor_of("cmd"), Some(3));
        }
        {
            let again = WindowJournal::with_cache(&root, "test-endpoint");
            assert_eq!(again.stamp_of("cmd"), Some((42, 9)));
            assert_eq!(again.floor_of("cmd"), Some(3));
        }
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn repoint_forgets_the_old_endpoints_facts() {
        let at = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!("gongga-repoint-{}-{at}", std::process::id()));
        let mut journal = WindowJournal::with_cache(&root, "one");
        journal.stamp("cmd", 1, 5);
        assert_eq!(journal.stamp_of("cmd"), Some((1, 5)));
        journal.repoint(&root, "two");
        assert_eq!(journal.stamp_of("cmd"), None);
        let _ = std::fs::remove_dir_all(&root);
    }
}
