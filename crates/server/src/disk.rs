//! What the device has left, and the one time a server says so.
//!
//! See `docs/notes/server-disk.md`.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{OnceLock, RwLock};

/// The event kind a low disk reports itself with: a fact in the log, not a message.
pub const EVENT: &str = "disk_space";

/// What this process watches: the threshold, and the home's own volume.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Watcher {
    pub threshold: u64,
    pub home: PathBuf,
}

/// The watched setting for this process, if any.
///
/// # Invariant
///
/// One home per process, so the threshold is one value per process: a field on
/// every tool call would be the same value reached by a longer path. A lock rather
/// than a `OnceLock` value, because a test installs its own and takes it away.
fn slot() -> &'static RwLock<Option<Watcher>> {
    static SLOT: OnceLock<RwLock<Option<Watcher>>> = OnceLock::new();
    SLOT.get_or_init(|| RwLock::new(None))
}

pub fn install(threshold: u64, home: impl Into<PathBuf>) {
    if let Ok(mut slot) = slot().write() {
        *slot = Some(Watcher {
            threshold,
            home: home.into(),
        });
    }
}

pub fn installed() -> Option<Watcher> {
    slot().read().ok().and_then(|slot| slot.clone())
}

/// Forgets the setting, so a test's is not carried into the next one.
#[cfg(test)]
pub fn uninstall() {
    if let Ok(mut slot) = slot().write() {
        *slot = None;
    }
}

/// Whether this server is already inside an episode it has reported.
///
/// # Invariant
///
/// The episode is a fact about *this server*, not about the device: the room left
/// is read on every tool call, and a disk that stays small would otherwise report
/// itself on every one of them. Nothing durable and no shared file -- a restart
/// reads the disk again and says so once, which is what a restart should do.
fn reported() -> &'static AtomicBool {
    static REPORTED: OnceLock<AtomicBool> = OnceLock::new();
    REPORTED.get_or_init(|| AtomicBool::new(false))
}

/// Reads the room left after a tool call, and writes the fact if it is new.
///
/// # Invariant
///
/// Every tool call ends here, not only a failed one: measured, a disk that is down
/// to its last room serves ordinary commands happily -- a `cat`, an `ls`, a shell
/// that only reads -- so waiting for a failure waits for the first write that
/// happens to be big enough, which on a server that mostly reads may be never.
/// See `docs/notes/server-disk.md`.
pub fn note_tool_end(store: &crate::store::Store, session: &str, tool: &str, cwd: &Path, ok: bool) {
    let Some(watcher) = installed() else {
        return;
    };
    if watcher.threshold == 0 {
        return;
    }
    let volumes = volumes(&watcher.home);
    let free = volumes.iter().filter_map(|volume| volume.free).min();
    if !low(free, watcher.threshold) {
        reported().store(false, Ordering::SeqCst);
        return;
    }
    // The edge only: an episode already reported stays quiet until the disk is
    // seen above the threshold again.
    if reported().swap(true, Ordering::SeqCst) {
        return;
    }
    let payload = serde_json::json!({
        "session": session,
        "tool": tool,
        "tool_ok": ok,
        "cwd": cwd.display().to_string(),
        "threshold": watcher.threshold,
        "volumes": volumes
            .into_iter()
            .map(|volume| serde_json::json!({
                "label": volume.label,
                "free": volume.free,
            }))
            .collect::<Vec<_>>(),
    });
    match store.append_event_row(None, EVENT, &payload) {
        Ok(row) => tracing::info!(
            session,
            tool,
            event = row.id,
            threshold = watcher.threshold,
            free = ?free,
            "the disk is below the threshold this machine declared"
        ),
        Err(e) => {
            tracing::warn!(error = %e, "could not record a low disk");
            // Not reported then: the next call may say so.
            reported().store(false, Ordering::SeqCst);
        }
    }
}

/// Whether the smallest of what the volumes reported is below the threshold.
///
/// # Invariant
///
/// The smallest, not the average and not the home's own: a tool writes wherever its
/// command says, so the volume that is out of room is whichever one answers lowest.
fn low(free: Option<u64>, threshold: u64) -> bool {
    free.is_some_and(|free| free < threshold)
}

/// One volume this server could lose room on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Volume {
    pub label: String,
    pub free: Option<u64>,
}

/// The volumes this server could lose room on: every fixed drive, and the home's
/// own volume even when it is neither.
///
/// # Invariant
///
/// The home's volume is the one that matters -- the database, the log, spilled tool
/// output and job records are all there -- and the rest are reported so a monitor
/// starts with the map instead of spending its first tool call on a survey.
pub fn volumes(home: &Path) -> Vec<Volume> {
    let mut out = fixed_volumes();
    let home_volume = volume_of(home);
    if let Some(home_volume) = home_volume
        && !out.iter().any(|volume| volume.label == home_volume.label)
    {
        out.push(home_volume);
    }
    out
}

/// Every fixed drive this machine has, with the room left on it.
#[cfg(windows)]
fn fixed_volumes() -> Vec<Volume> {
    use windows_sys::Win32::Storage::FileSystem::{GetDriveTypeW, GetLogicalDrives};
    use windows_sys::Win32::System::WindowsProgramming::DRIVE_FIXED;

    let mut out = Vec::new();
    let mask = unsafe { GetLogicalDrives() };
    for bit in 0..26u32 {
        if mask & (1 << bit) == 0 {
            continue;
        }
        let letter = (b'A' + bit as u8) as char;
        let root = format!("{letter}:\\");
        if unsafe { GetDriveTypeW(wide(&root).as_ptr()) } != DRIVE_FIXED {
            continue;
        }
        out.push(Volume {
            label: format!("{letter}:"),
            free: free_space(Path::new(&root)),
        });
    }
    out
}

/// Outside Windows: the home's own volume, and no mount table to read.
#[cfg(not(windows))]
fn fixed_volumes() -> Vec<Volume> {
    Vec::new()
}

/// The volume a path stands on, by the name that identifies it.
#[cfg(windows)]
fn volume_of(path: &Path) -> Option<Volume> {
    // Its drive letter: a mounted directory reads as its host, where the room is.
    let text = path.to_string_lossy();
    let letter = text.chars().next().filter(char::is_ascii_alphabetic)?;
    let label = format!("{}:", letter.to_ascii_uppercase());
    let root = format!("{letter}:\\");
    Some(Volume {
        label,
        free: free_space(Path::new(&root)),
    })
}

#[cfg(not(windows))]
fn volume_of(path: &Path) -> Option<Volume> {
    Some(Volume {
        label: "the home's volume".to_string(),
        free: free_space(path),
    })
}

/// Room left on the volume `path` stands on.
#[cfg(windows)]
pub fn free_space(path: &Path) -> Option<u64> {
    use windows_sys::Win32::Storage::FileSystem::GetDiskFreeSpaceExW;

    let mut available: u64 = 0;
    let ok = unsafe {
        GetDiskFreeSpaceExW(
            wide(&path.to_string_lossy()).as_ptr(),
            &mut available,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        )
    };
    (ok != 0).then_some(available)
}

#[cfg(unix)]
pub fn free_space(path: &Path) -> Option<u64> {
    use std::os::unix::ffi::OsStrExt;

    let c = std::ffi::CString::new(path.as_os_str().as_bytes()).ok()?;
    let mut stat: libc::statvfs = unsafe { std::mem::zeroed() };
    let ok = unsafe { libc::statvfs(c.as_ptr(), &mut stat) };
    (ok == 0).then(|| u64::from(stat.f_bavail).saturating_mul(u64::from(stat.f_frsize)))
}

#[cfg(not(any(unix, windows)))]
pub fn free_space(_path: &Path) -> Option<u64> {
    None
}

/// A byte count as a reader holds it: what a log line and a report need.
pub fn human(bytes: u64) -> String {
    const UNITS: [(&str, u64); 4] = [
        ("GiB", 1024 * 1024 * 1024),
        ("MiB", 1024 * 1024),
        ("KiB", 1024),
        ("B", 1),
    ];
    for (unit, size) in UNITS {
        if bytes >= size {
            if size == 1 {
                return format!("{bytes} B");
            }
            return format!("{:.1} {unit}", bytes as f64 / size as f64);
        }
    }
    "0 B".to_string()
}

#[cfg(windows)]
fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}

#[cfg(test)]
mod tests;
