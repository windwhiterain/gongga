//! The window's own record of the rows it was given, on disk.
//!
//! # Invariant
//!
//! A client's events are the client's: this module never asks the server where they
//! live or whether it may keep them, and nothing here is derived from a greeting.
//! See `docs/notes/tui-cache.md`.

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
    ///
    /// # Invariant
    ///
    /// Only an older build writes one: a stream now lives in memory until its seal,
    /// and a journal holds a block's whole form or nothing. It stays in the
    /// vocabulary because the journals already on disk are full of them, and a
    /// record a build cannot name is a record it cannot read past.
    Delta {
        id: u64,
        kind: LineKind,
        tag: String,
        text: String,
        done: bool,
    },
    /// One session's transcript now ends at `seq`, and every block recorded
    /// above this record was drawn from a stored row.
    ///
    /// # Invariant
    ///
    /// A window's watermark, and the only thing a later greeting is compared
    /// with: `created` is the session's, so a name the server handed out again is
    /// not mistaken for the transcript this one drew. See `docs/notes/tui-cache.md`.
    Stamp { created: i64, seq: i64 },
    /// Everything above the last `Stamp` is thrown away: the server resends it.
    ///
    /// # Invariant
    ///
    /// Written before the refill's blocks, and read by the replay that rebuilds the
    /// window: a restart mid-refill neither loses the rows below the stamp nor keeps
    /// the half-written block it replaced.
    Rewind,
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

/// How many bytes frame a record: a length in front, and the same length behind.
///
/// # Invariant
///
/// The trailing copy is what makes a walk backwards possible at all: a record's
/// length is otherwise only knowable from its front, so reading the newest records
/// would mean walking the whole file forwards.
const FRAME: u64 = 4;

/// The framing a journal is written in. Bumped when `Record` changes shape.
const VERSION: u32 = 2;

/// One window's journal, held open for as long as this process draws it.
///
/// # Invariant
///
/// The open handle is what makes a record affordable: opening a file on the machine
/// this was written on costs about ten milliseconds, a write on a handle about three
/// microseconds, so a journal reopened per record turns a stream into a stall.
/// See `docs/notes/tui-cache.md`.
struct Slot {
    path: PathBuf,
    /// `None` until the first read or write, and again after a reset or a removal.
    file: Option<File>,
}

impl Slot {
    /// The file this window would use, named after this process.
    fn new(dir: &Path, window: &str) -> Self {
        Self {
            path: dir.join(format!(
                "{}.{}.journal",
                escaped(window),
                std::process::id()
            )),
            file: None,
        }
    }
}

/// The journals of one client, over one server address.
///
/// # Invariant
///
/// One writer per window, and the writer is a *process*: a journal is named after
/// the pid that writes it, so two windows on one machine never append to the same
/// file, and a restart takes over the file the dead process left. See
/// `docs/notes/tui-cache.md`.
pub struct Cache {
    /// Where this client keeps them, or `None` for a log that keeps nothing.
    dir: Option<PathBuf>,
    /// Window -> the journal this process uses for it. Decided once, by `new`.
    journals: std::sync::Mutex<std::collections::HashMap<String, Slot>>,
}

impl Cache {
    /// The journals for one endpoint, under a root this client owns.
    ///
    /// # Invariant
    ///
    /// The root is the client's, and the endpoint comes from this client's own
    /// configuration: a greeting would make the place a block lives depend on the
    /// server it came from. See `docs/notes/tui-cache.md`.
    pub fn new(root: &Path, endpoint: &str) -> Self {
        let dir = root.join("events").join(escaped(endpoint));
        let journals = Self::claim(&dir);
        Self {
            dir: Some(dir),
            journals: std::sync::Mutex::new(journals),
        }
    }

    /// A cache that keeps nothing: a log built with no front-end in mind.
    ///
    /// # Invariant
    ///
    /// No directory is derived from an empty root, so nothing is ever created in
    /// whatever directory the process happens to stand in.
    pub fn none() -> Self {
        Self {
            dir: None,
            journals: std::sync::Mutex::new(std::collections::HashMap::new()),
        }
    }

    /// Decides which file each window uses: an old one if its writer is gone.
    ///
    /// # Invariant
    ///
    /// A journal whose writer is alive is never taken over -- that is the whole
    /// reason the pid is in the name -- and the newest abandoned one is renamed to
    /// this process, which keeps a session at one file however often the window was
    /// opened. A rename that loses a race is no error: the loser writes a file of
    /// its own and the winner's records stand. See `docs/notes/tui-cache.md`.
    fn claim(dir: &Path) -> std::collections::HashMap<String, Slot> {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return std::collections::HashMap::new();
        };
        let mut found: std::collections::HashMap<String, Vec<One>> =
            std::collections::HashMap::new();
        for entry in entries.flatten() {
            let path = entry.path();
            let Some((window, pid)) = split_name(&path) else {
                continue;
            };
            let written = entry
                .metadata()
                .and_then(|meta| meta.modified())
                .ok()
                .and_then(|at| at.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|age| age.as_nanos())
                .unwrap_or(0);
            found
                .entry(window)
                .or_default()
                .push(One { pid, written, path });
        }
        let me = std::process::id() as u64;
        let mut ask = Ask::default();
        let mut journals = std::collections::HashMap::new();
        for (window, mut files) in found {
            files.sort_by_key(|one| one.written);
            let mut chosen: Option<PathBuf> = None;
            if let Some(mine) = files.iter().find(|one| one.pid == me) {
                chosen = Some(mine.path.clone());
            } else if let Some(newest) = files.last()
                && !ask.live(newest.pid)
            {
                let mine = mine_path(&newest.path, me);
                if std::fs::rename(&newest.path, &mine).is_ok() {
                    chosen = Some(mine);
                }
            }
            if let Some(path) = chosen.clone() {
                journals.insert(window.clone(), Slot { path, file: None });
            }
            // What is left over belongs to nobody: a pid says a process that ran.
            for one in &files {
                if Some(&one.path) == chosen.as_ref() || ask.live(one.pid) {
                    continue;
                }
                let _ = std::fs::remove_file(&one.path);
            }
        }
        journals
    }

    /// Runs `f` on this window's journal, opening it once and keeping it open.
    ///
    /// # Invariant
    ///
    /// One handle per window for the life of the process: that is the whole cost
    /// model of this module. A read and a write share it, and the lock makes the
    /// seek and the transfer one step, so two windows never interleave.
    /// See `docs/notes/tui-cache.md`.
    fn with<T>(
        &self,
        window: &str,
        create: bool,
        f: impl FnOnce(&mut File) -> Option<T>,
    ) -> Option<T> {
        let dir = self.dir.as_ref()?;
        let mut journals = self
            .journals
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let slot = journals
            .entry(window.to_string())
            .or_insert_with(|| Slot::new(dir, window));
        if slot.file.is_none() {
            if create {
                std::fs::create_dir_all(dir).ok()?;
            }
            let mut open = OpenOptions::new();
            open.read(true).write(true);
            if create {
                open.create(true);
            }
            slot.file = open.open(&slot.path).ok();
        }
        f(slot.file.as_mut()?)
    }

    /// Where this window's journal lives, whether or not it is open.
    fn path(&self, window: &str) -> Option<PathBuf> {
        let dir = self.dir.as_ref()?;
        let mut journals = self
            .journals
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        Some(
            journals
                .entry(window.to_string())
                .or_insert_with(|| Slot::new(dir, window))
                .path
                .clone(),
        )
    }

    /// Drops this process's handle, so the file can be truncated or deleted.
    fn close(&self, window: &str) {
        let mut journals = self
            .journals
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(slot) = journals.get_mut(window) {
            slot.file = None;
        }
    }

    /// Appends one record, and answers where it was written.
    ///
    /// # Invariant
    ///
    /// The offset is what a `Seal` is found by later, so it is the record's own
    /// start: a caller that keeps it can read that block back without its neighbours.
    pub fn append(&self, window: &str, record: &Record) -> Option<u64> {
        self.dir.as_ref()?;
        let mut body = Vec::new();
        body.extend_from_slice(&VERSION.to_le_bytes());
        body.extend_from_slice(&serde_json::to_vec(record).ok()?);
        let len = (body.len() as u32).to_le_bytes();
        let mut framed = Vec::with_capacity(body.len() + 8);
        framed.extend_from_slice(&len);
        framed.extend_from_slice(&body);
        framed.extend_from_slice(&len);
        self.with(window, true, |file| {
            let at = file.seek(SeekFrom::End(0)).ok()?;
            file.write_all(&framed).ok()?;
            Some(at)
        })
    }

    /// Writes down that the window has drawn up to `seq` of the session.
    pub fn stamp(&self, window: &str, created: i64, seq: i64) {
        self.append(window, &Record::Stamp { created, seq });
    }

    /// Writes down that the tail above the last stamp is thrown away.
    pub fn rewind(&self, window: &str) {
        self.append(window, &Record::Rewind);
    }

    /// Where this window's journal ends: the offset a later record starts at.
    ///
    /// # Invariant
    ///
    /// Asks the file system rather than opening the file: a frame asks this for every
    /// window it draws, and a window that has written nothing yet has no file to open.
    pub fn end(&self, window: &str) -> u64 {
        let Some(path) = self.path(window) else {
            return 0;
        };
        std::fs::metadata(path).map(|meta| meta.len()).unwrap_or(0)
    }

    /// The record written at `at`, if one is there and this build can read it.
    pub fn read(&self, window: &str, at: u64) -> Option<Record> {
        self.with(window, false, |file| {
            file.seek(SeekFrom::Start(at)).ok()?;
            Self::record_at(file).map(|(_, record)| record)
        })
    }

    /// Where the `count` records before `from` begin, or 0 when the file starts first.
    pub fn back(&self, window: &str, from: u64, count: usize) -> u64 {
        self.with(window, false, |file| {
            let end = file.seek(SeekFrom::End(0)).ok()?;
            let mut at = from.min(end);
            for _ in 0..count {
                at = Self::previous(file, at)?;
            }
            Some(at)
        })
        .unwrap_or(0)
    }

    /// The newest stamp of a window: where it was written, and what it said.
    ///
    /// # Invariant
    ///
    /// `at` is the record's own offset, which is what a refill cuts at: a block
    /// whose record was written above this one was drawn after the last row the
    /// stamp accounts for. Walking back stops at the first record this build
    /// cannot read, for the same reason every other backward walk does.
    pub fn stamp_at(&self, window: &str, before: u64) -> Option<(u64, i64, i64)> {
        let mut at = before.min(self.end(window));
        while at > 0 {
            let start = self.back(window, at, 1);
            if let Some(Record::Stamp { created, seq }) = self.read(window, start) {
                return Some((start, created, seq));
            }
            at = start;
        }
        None
    }

    /// The record just before `at`, if the file holds one this build understands.
    fn previous(file: &mut File, at: u64) -> Option<u64> {
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
        Self::decode(&body).map(|_| start)
    }

    /// Every record from `from` to the end, oldest first.
    pub fn forward(&self, window: &str, from: u64) -> Vec<(u64, Record)> {
        self.with(window, false, |file| {
            let end = file.seek(SeekFrom::End(0)).ok()?;
            let mut at = from;
            let mut out = Vec::new();
            while at < end {
                file.seek(SeekFrom::Start(at)).ok()?;
                match Self::record_at(file) {
                    Some((next, record)) => {
                        out.push((at, record));
                        at = next;
                    }
                    None => break,
                }
            }
            Some(out)
        })
        .unwrap_or_default()
    }

    /// One record read forwards at the handle's position, and where the next starts.
    fn record_at(file: &mut File) -> Option<(u64, Record)> {
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
        Self::decode(&body).map(|record| (at + FRAME + len as u64 + FRAME, record))
    }

    /// One framed body as a record: the version first, so a foreign one is refused.
    fn decode(body: &[u8]) -> Option<Record> {
        let (version, json) = body.split_first_chunk::<4>()?;
        if u32::from_le_bytes(*version) != VERSION {
            return None;
        }
        serde_json::from_slice(json).ok()
    }

    /// Forgets everything this window drew.
    ///
    /// # Invariant
    ///
    /// This process's file, and every abandoned one: the window's history *is* the
    /// file, so emptying it is the statement `clear` makes. A file another live
    /// client is writing is left alone -- it is that client's record.
    pub fn reset(&self, window: &str) {
        let Some(path) = self.path(window) else {
            return;
        };
        // Closed first: a truncate behind an open handle is a write at the wrong offset.
        self.close(window);
        let _ = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(&path);
        self.forget_abandoned(window, &path);
    }

    /// Drops a window's journals: the session it belonged to is gone.
    pub fn remove(&self, window: &str) {
        let Some(path) = self.path(window) else {
            return;
        };
        // Windows refuses to delete a file this process still holds open.
        self.close(window);
        let _ = std::fs::remove_file(&path);
        self.forget_abandoned(window, Path::new(""));
    }

    /// Deletes the files of `window` whose writer is gone, keeping `keep`.
    fn forget_abandoned(&self, window: &str, keep: &Path) {
        let Some(dir) = self.dir.as_ref() else {
            return;
        };
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        let mut ask = Ask::default();
        for entry in entries.flatten() {
            let path = entry.path();
            let Some((name, pid)) = split_name(&path) else {
                continue;
            };
            if name != window || path == keep {
                continue;
            }
            let live = ask.live(pid);
            if !live {
                let _ = std::fs::remove_file(&path);
            }
        }
    }
}

/// Whether a pid answers, asked once per pid for one walk of the directory.
///
/// # Invariant
///
/// `cli::server::alive` starts a process on Windows -- the first call on this machine
/// costs about a third of a second -- and one walk asks about the same few pids once
/// per file. An answer cannot change inside a walk: a pid that is gone stays gone.
#[derive(Default)]
struct Ask(std::collections::HashMap<u64, bool>);

impl Ask {
    /// Whether `pid` is a process, asking the machine at most once for it.
    fn live(&mut self, pid: u64) -> bool {
        *self
            .0
            .entry(pid)
            .or_insert_with(|| cli::server::alive(pid as u32))
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

/// One journal found in the directory: whose it is, and how recent it is.
struct One {
    pid: u64,
    written: u128,
    path: PathBuf,
}

/// `<window>.<pid>.journal` split back into its two parts.
fn split_name(path: &Path) -> Option<(String, u64)> {
    let name = path.file_name()?.to_str()?;
    let stem = name.strip_suffix(".journal")?;
    let (window, pid) = stem.rsplit_once('.')?;
    let pid = pid.parse::<u64>().ok()?;
    Some((unescaped(window), pid))
}

/// The reverse of [`escaped`], so a file name is read back as the session's name.
fn unescaped(name: &str) -> String {
    let bytes = name.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut at = 0;
    while at < bytes.len() {
        match bytes[at] {
            b'%' if at + 2 < bytes.len() => {
                let hex = std::str::from_utf8(&bytes[at + 1..at + 3]).ok();
                match hex.and_then(|hex| u8::from_str_radix(hex, 16).ok()) {
                    Some(byte) => {
                        out.push(byte);
                        at += 3;
                    }
                    None => {
                        out.push(bytes[at]);
                        at += 1;
                    }
                }
            }
            byte => {
                out.push(byte);
                at += 1;
            }
        }
    }
    String::from_utf8(out).unwrap_or_else(|_| name.to_string())
}

/// The same journal under this process's pid.
fn mine_path(path: &Path, pid: u64) -> PathBuf {
    let stem = path
        .file_name()
        .and_then(|name| name.to_str())
        .and_then(|name| name.strip_suffix(".journal"))
        .and_then(|stem| stem.rsplit_once('.').map(|(window, _)| window))
        .unwrap_or_default();
    path.with_file_name(format!("{stem}.{pid}.journal"))
}
