//! The window's own copy of what it drew, on disk.
//!
//! # Invariant
//!
//! The journal is the truth and the scrollback is a view of it: a block is written
//! before it is drawn, and what memory drops can be read back. See
//! `docs/notes/tui-cache.md`.

use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::LineKind;
use cli::protocol;

/// One thing that happened to one window's blocks, in the order it happened.
///
/// # Invariant
///
/// A `Seal` is self-sufficient: it carries the whole block, so reading it is enough
/// to draw that block with no earlier record. A `Delta` is only ever the tail of a
/// block that is still open, which is why a block never needs its whole `Delta`
/// history to be read back.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "k", rename_all = "snake_case")]
pub enum Record {
    /// A line that arrived whole: sealed as it lands.
    Line {
        id: u64,
        kind: LineKind,
        text: String,
    },
    /// One more piece of a block that is still growing.
    Delta {
        id: u64,
        kind: LineKind,
        tag: String,
        text: String,
        done: bool,
    },
    /// A block as it stands: created, relabelled, described, opened or closed.
    Seal {
        id: u64,
        kind: LineKind,
        tag: String,
        key: String,
        text: String,
        expanded: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        call: Option<protocol::Head>,
    },
}

impl Record {
    /// The block this record is about, when it is about one.
    pub fn id(&self) -> Option<u64> {
        match self {
            Record::Line { id, .. } | Record::Delta { id, .. } | Record::Seal { id, .. } => {
                Some(*id)
            }
        }
    }
}

/// How many bytes frame a record: a length in front, and the same length behind.
///
/// # Invariant
///
/// The trailing copy is what makes a walk backwards possible at all: a record's
/// length is otherwise only knowable from its front, so reading the newest records
/// would mean walking the whole file forwards.
const FRAME: u64 = 4;

/// The journal of every window this process has written or read.
///
/// # Invariant
///
/// One file per window, and one writer: a window that does not own the server it
/// reads from keeps its blocks in memory and writes nothing, so two windows on one
/// home cannot interleave their records. See `docs/notes/tui-cache.md`.
pub struct Cache {
    dir: PathBuf,
    /// Whether this window may write. A reader still reads.
    writable: bool,
    /// The version this build writes and the only one it reads.
    version: u32,
}

/// The framing a journal is written in. Bumped when `Record` changes shape.
const VERSION: u32 = 1;

impl Cache {
    /// The cache under this home; one that cannot be written leaves the window
    /// as it was before there was a cache.
    pub fn new(home: &Path) -> Self {
        Self {
            dir: home.join("cache"),
            writable: false,
            version: VERSION,
        }
    }

    /// Says whether this window owns the server it draws, and so may write.
    ///
    /// # Invariant
    ///
    /// Only the owning window writes: the server has one owner, so ownership is the
    /// only thing two windows on one home already agree about.
    pub fn set_writable(&mut self, writable: bool) {
        self.writable = writable;
    }

    fn path(&self, window: &str) -> PathBuf {
        self.dir.join(format!("{}.journal", escaped(window)))
    }

    /// Appends one record, and answers where it was written.
    ///
    /// # Invariant
    ///
    /// The offset is what a `Seal` is found by later, so it is the record's own
    /// start: a caller that keeps it can read that block back without its neighbours.
    pub fn append(&self, window: &str, record: &Record) -> Option<u64> {
        if !self.writable {
            return None;
        }
        let mut body = Vec::new();
        body.extend_from_slice(&self.version.to_le_bytes());
        match serde_json::to_vec(record) {
            Ok(json) => body.extend_from_slice(&json),
            Err(_) => return None,
        }
        let Ok(mut file) = self.open_for_append(window) else {
            return None;
        };
        let at = file.seek(SeekFrom::End(0)).ok()?;
        let len = (body.len() as u32).to_le_bytes();
        let mut framed = Vec::with_capacity(body.len() + 8);
        framed.extend_from_slice(&len);
        framed.extend_from_slice(&body);
        framed.extend_from_slice(&len);
        file.write_all(&framed).ok()?;
        Some(at)
    }

    fn open_for_append(&self, window: &str) -> std::io::Result<File> {
        std::fs::create_dir_all(&self.dir)?;
        OpenOptions::new()
            .create(true)
            .append(true)
            .read(true)
            .open(self.path(window))
    }

    /// Where this window's journal ends: the offset a later record starts at.
    pub fn end(&self, window: &str) -> u64 {
        File::open(self.path(window))
            .and_then(|file| file.metadata())
            .map(|meta| meta.len())
            .unwrap_or(0)
    }

    /// The record written at `at`, if one is there and this build can read it.
    pub fn read(&self, window: &str, at: u64) -> Option<Record> {
        let mut file = File::open(self.path(window)).ok()?;
        file.seek(SeekFrom::Start(at)).ok()?;
        self.record_at(&mut file).map(|(_, record)| record)
    }

    /// Where the `count` records before `from` begin, or 0 when the file starts first.
    pub fn back(&self, window: &str, from: u64, count: usize) -> u64 {
        let Ok(mut file) = File::open(self.path(window)) else {
            return 0;
        };
        let mut at = from.min(self.end(window));
        for _ in 0..count {
            match self.previous(&mut file, at) {
                Some(start) => at = start,
                None => return 0,
            }
        }
        at
    }

    /// The record just before `at`, if the file holds one this build understands.
    fn previous(&self, file: &mut File, at: u64) -> Option<u64> {
        if at < FRAME {
            return None;
        }
        let mut tail = [0u8; 4];
        file.seek(SeekFrom::Start(at - FRAME)).ok()?;
        file.read_exact(&mut tail).ok()?;
        let len = u32::from_le_bytes(tail) as u64;
        let start = at.checked_sub(FRAME + len + FRAME)?;
        // A body this build cannot read ends the walk: what precedes a record it
        // cannot parse is not addressable from the end.
        let mut body = vec![0u8; len as usize];
        file.seek(SeekFrom::Start(start + FRAME)).ok()?;
        file.read_exact(&mut body).ok()?;
        self.decode(&body).map(|_| start)
    }

    /// Every record from `from` to the end, oldest first.
    pub fn forward(&self, window: &str, from: u64) -> Vec<(u64, Record)> {
        let Ok(mut file) = File::open(self.path(window)) else {
            return Vec::new();
        };
        let end = self.end(window);
        let mut at = from;
        let mut out = Vec::new();
        while at < end {
            file.seek(SeekFrom::Start(at)).ok();
            match self.record_at(&mut file) {
                Some((next, record)) => {
                    out.push((at, record));
                    at = next;
                }
                None => break,
            }
        }
        out
    }

    /// One record read forwards at the handle's position, and where the next starts.
    fn record_at(&self, file: &mut File) -> Option<(u64, Record)> {
        let at = file.stream_position().ok()?;
        let mut head = [0u8; 4];
        if file.read_exact(&mut head).is_err() {
            return None;
        }
        let len = u32::from_le_bytes(head) as usize;
        let mut body = vec![0u8; len];
        file.read_exact(&mut body).ok()?;
        let mut tail = [0u8; 4];
        file.read_exact(&mut tail).ok()?;
        if u32::from_le_bytes(tail) as usize != len {
            return None;
        }
        self.decode(&body)
            .map(|record| (at + FRAME + len as u64 + FRAME, record))
    }

    /// One framed body as a record: the version first, so a foreign one is refused.
    fn decode(&self, body: &[u8]) -> Option<Record> {
        let (version, json) = body.split_first_chunk::<4>()?;
        if u32::from_le_bytes(*version) != self.version {
            return None;
        }
        serde_json::from_slice(json).ok()
    }

    /// Forgets everything a window drew, keeping the file for what comes next.
    ///
    /// # Invariant
    ///
    /// A truncation rather than a `Clear` record: the window's history *is* the
    /// file, so emptying it is the same statement and keeps it from growing.
    pub fn reset(&self, window: &str) {
        if !self.writable {
            return;
        }
        let _ = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(self.path(window));
    }

    /// Drops a window's journal: the session it belonged to is gone.
    pub fn remove(&self, window: &str) {
        if !self.writable {
            return;
        }
        let _ = std::fs::remove_file(self.path(window));
    }
}

/// A window's name as a file name: the one place a session's name meets a path.
///
/// # Invariant
///
/// Reversible and injective over the bytes a name may hold, because two windows that
/// differ only in a character must not share one journal -- and a name is not a file
/// name: `cmd ` ends in a space, and a session may be called anything.
fn escaped(window: &str) -> String {
    let mut out = String::with_capacity(window.len());
    for byte in window.bytes() {
        match byte {
            b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' | b'-' | b'_' => out.push(byte as char),
            other => out.push_str(&format!("%{other:02X}")),
        }
    }
    out
}
