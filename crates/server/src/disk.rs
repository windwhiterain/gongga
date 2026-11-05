//! The device's safety space, and the claim on it that says who noticed first.
//!
//! See `docs/notes/server-disk.md`.

use std::path::{Path, PathBuf};
use std::sync::{OnceLock, RwLock};

/// The reserve file's name: one per device, so two servers on one machine share
/// one claim rather than each keeping a copy.
pub const FILE: &str = "gongga-disk-reserve";

/// The file name for a machine that states none. A machine-wide directory, never a
/// home: the whole point is that two servers with different homes spend the same
/// space.
///
/// # Invariant
///
/// `GONGGA_DISK_RESERVE` states the whole path, directory included, so a test can
/// spend a file of its own instead of the one this machine's servers share.
pub fn default_path() -> PathBuf {
    if let Some(stated) = std::env::var_os("GONGGA_DISK_RESERVE")
        && !stated.is_empty()
    {
        return PathBuf::from(stated);
    }
    if cfg!(windows) {
        let root = std::env::var_os("ProgramData")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("ALLUSERSPROFILE").map(PathBuf::from))
            .unwrap_or_else(|| PathBuf::from("C:/ProgramData"));
        root.join("gongga").join(FILE)
    } else if Path::new("/var/tmp").is_dir() {
        Path::new("/var/tmp").join("gongga").join(FILE)
    } else {
        std::env::temp_dir().join("gongga").join(FILE)
    }
}

/// The space this machine set aside, and where it stands.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reserve {
    path: PathBuf,
    bytes: u64,
}

/// What a claim did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Claim {
    /// This caller deleted the file, so this caller spends it and wakes the
    /// monitor.
    Won,
    /// Somebody else deleted it first: the disk is already somebody's business,
    /// and this caller says nothing.
    Lost,
    /// The file could not be removed for a reason that is not "it was gone".
    Failed(String),
}

/// One volume a tool could lose room on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Volume {
    pub label: String,
    pub free: Option<u64>,
}

impl Reserve {
    pub fn new(path: impl Into<PathBuf>, bytes: u64) -> Self {
        Self {
            path: path.into(),
            bytes,
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn bytes(&self) -> u64 {
        self.bytes
    }

    /// Puts the space back: creates the file and takes the blocks.
    ///
    /// # Invariant
    ///
    /// Creating is `create_new`, so exactly one server arms a device; the others
    /// find the file and leave it alone. Idempotent: calling it on an armed file
    /// either succeeds by doing nothing or grows a file a killed writer left short.
    /// A file that cannot be grown is removed again -- half a reserve is worse
    /// than none, because the yardstick would be a size nobody declared.
    pub fn arm(&self) -> Result<(), String> {
        if let Some(parent) = self.path.parent()
            && let Err(e) = std::fs::create_dir_all(parent)
        {
            return Err(format!("{}: {e}", parent.display()));
        }
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&self.path)
        {
            Ok(file) => {
                if let Err(e) = file.set_len(self.bytes) {
                    drop(file);
                    let _ = std::fs::remove_file(&self.path);
                    return Err(format!("{}: {e}", self.path.display()));
                }
                Ok(())
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => self.grow(),
            Err(e) => Err(format!("{}: {e}", self.path.display())),
        }
    }

    fn grow(&self) -> Result<(), String> {
        let Ok(meta) = std::fs::metadata(&self.path) else {
            return Ok(());
        };
        if meta.len() >= self.bytes {
            return Ok(());
        }
        let file = std::fs::OpenOptions::new()
            .write(true)
            .open(&self.path)
            .map_err(|e| format!("{}: {e}", self.path.display()))?;
        file.set_len(self.bytes)
            .map_err(|e| format!("{}: {e}", self.path.display()))
    }

    /// Takes the space back, and with it the right to say anything.
    ///
    /// # Invariant
    ///
    /// The check is the delete, never a read before it: a read leaves a window in
    /// which two servers both believe they won, and the space is then mentioned
    /// twice. What "the delete" means is the platform's answer --
    /// [unix](Self::claim_unix), [windows](Self::claim_windows).
    pub fn claim(&self) -> Claim {
        #[cfg(unix)]
        {
            self.claim_unix()
        }
        #[cfg(windows)]
        {
            self.claim_windows()
        }
        #[cfg(not(any(unix, windows)))]
        {
            match std::fs::remove_file(&self.path) {
                Ok(()) => Claim::Won,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => Claim::Lost,
                Err(e) => Claim::Failed(format!("{}: {e}", self.path.display())),
            }
        }
    }

    /// `unlink` is both the check and the delete: exactly one caller sees `Ok`, and
    /// everybody else sees "no such file".
    #[cfg(unix)]
    fn claim_unix(&self) -> Claim {
        match std::fs::remove_file(&self.path) {
            Ok(()) => Claim::Won,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Claim::Lost,
            Err(e) => Claim::Failed(format!("{}: {e}", self.path.display())),
        }
    }

    /// # Invariant
    ///
    /// An exclusive open, not a delete. Measured: two callers can open one file and
    /// both report a successful `DeleteFileW` -- the disposition is applied to a
    /// handle that was already opened, so the name is not what is decided. A handle
    /// with no sharing is: the first caller gets it, everybody else gets a sharing
    /// violation, and `FILE_FLAG_DELETE_ON_CLOSE` makes the file go away with the
    /// handle that won it.
    #[cfg(windows)]
    fn claim_windows(&self) -> Claim {
        use windows_sys::Win32::Foundation::{CloseHandle, INVALID_HANDLE_VALUE};
        use windows_sys::Win32::Storage::FileSystem::{
            CreateFileW, DELETE, FILE_ATTRIBUTE_NORMAL, FILE_FLAG_DELETE_ON_CLOSE, OPEN_EXISTING,
        };

        const ERROR_SHARING_VIOLATION: i32 = 32;
        const ATTEMPTS: u32 = 3;
        let path = wide(&self.path.to_string_lossy());
        for attempt in 0..ATTEMPTS {
            let handle = unsafe {
                CreateFileW(
                    path.as_ptr(),
                    DELETE,
                    0,
                    std::ptr::null(),
                    OPEN_EXISTING,
                    FILE_ATTRIBUTE_NORMAL | FILE_FLAG_DELETE_ON_CLOSE,
                    std::ptr::null_mut(),
                )
            };
            if handle != INVALID_HANDLE_VALUE {
                unsafe { CloseHandle(handle) };
                return Claim::Won;
            }
            let error = std::io::Error::last_os_error();
            match error.raw_os_error() {
                // ERROR_FILE_NOT_FOUND, ERROR_PATH_NOT_FOUND: somebody got here first.
                Some(2) | Some(3) => return Claim::Lost,
                // A scanner holding the file is not a claim either way: ask again.
                Some(ERROR_SHARING_VIOLATION) if attempt + 1 < ATTEMPTS => {
                    std::thread::sleep(std::time::Duration::from_millis(20));
                }
                _ => return Claim::Failed(format!("{}: {error}", self.path.display())),
            }
        }
        Claim::Failed(format!(
            "{}: still held by another process",
            self.path.display()
        ))
    }

    pub fn armed(&self) -> bool {
        self.path.exists()
    }
}

/// What this process watches: the device's reserve, and the home whose volume is
/// the one this server cannot do without.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Watcher {
    pub reserve: Reserve,
    pub home: PathBuf,
}

/// The event kind a spent reserve writes: a fact in the log, not a message.
pub const EVENT: &str = "disk_space";

/// The reserve this process states, if any.
///
/// # Invariant
///
/// One home per process, so the reserve is one value per process: a field on every
/// tool call would be the same value reached by a longer path. A lock rather than a
/// `OnceLock` value, because a test installs its own file and takes it away again.
fn slot() -> &'static RwLock<Option<Watcher>> {
    static SLOT: OnceLock<RwLock<Option<Watcher>>> = OnceLock::new();
    SLOT.get_or_init(|| RwLock::new(None))
}

pub fn install(reserve: Reserve, home: impl Into<PathBuf>) {
    if let Ok(mut slot) = slot().write() {
        *slot = Some(Watcher {
            reserve,
            home: home.into(),
        });
    }
}

pub fn installed() -> Option<Watcher> {
    slot().read().ok().and_then(|slot| slot.clone())
}

/// Forgets the reserve, so a test's file is not carried into the next one.
#[cfg(test)]
pub fn uninstall() {
    if let Ok(mut slot) = slot().write() {
        *slot = None;
    }
}

/// Writes the fact that this device's last room was spent, if it was.
///
/// # Invariant
///
/// Two readings of "the disk is full", and either is enough: the failure says so,
/// or a volume this server could write to has less room left than the reserve this
/// machine set aside. The second exists because the first is lost often -- through
/// a pipe, a swallowed stderr, a localized message -- and a tool that fails for an
/// unstated reason on a full disk is the case that matters.
///
/// Only the winner of the claim writes anything: one disk-full episode is one row,
/// and one row wakes one monitor, no matter how many servers noticed.
pub fn note_tool_failure(
    store: &crate::store::Store,
    session: &str,
    tool: &str,
    cwd: &Path,
    failure: &str,
) -> bool {
    let Some(watcher) = installed() else {
        return false;
    };
    if !watcher.reserve.armed() {
        // Disarmed: the episode is already somebody's, and nothing here adds to it.
        return false;
    }
    if !disk_full(failure) && !below_margin(&watcher.reserve, &watcher.home) {
        return false;
    }
    match watcher.reserve.claim() {
        Claim::Won => {
            let payload = serde_json::json!({
                "session": session,
                "tool": tool,
                "cwd": cwd.display().to_string(),
                "reserve": watcher.reserve.path().display().to_string(),
                "bytes": watcher.reserve.bytes(),
                "volumes": volumes(&watcher.home)
                    .into_iter()
                    .map(|volume| serde_json::json!({
                        "label": volume.label,
                        "free": volume.free,
                    }))
                    .collect::<Vec<_>>(),
                "failure": head(failure),
            });
            match store.append_event_row(None, EVENT, &payload) {
                Ok(row) => {
                    tracing::info!(
                        session,
                        tool,
                        event = row.id,
                        freed = watcher.reserve.bytes(),
                        "spent the device's disk reserve to report a full disk"
                    );
                    true
                }
                Err(e) => {
                    // The space is back, so the report is lost but the disk is not.
                    tracing::warn!(error = %e, "spent the disk reserve but could not record it");
                    false
                }
            }
        }
        Claim::Lost => false,
        Claim::Failed(why) => {
            // Neither won nor lost: say so, and do not wake a second monitor over it.
            tracing::warn!(why, "could not spend the disk reserve");
            false
        }
    }
}

/// The part of a tool's own words a report keeps: enough to recognise the cause,
/// short enough to ride a transcript.
fn head(failure: &str) -> String {
    const KEPT: usize = 2_000;
    if failure.chars().count() <= KEPT {
        return failure.to_string();
    }
    let mut out: String = failure.chars().take(KEPT).collect();
    out.push_str("\n[...]");
    out
}

/// Whether a failure's own words name a full disk.
///
/// # Invariant
///
/// A hint, never the only test: it is what the operating system and the common
/// implementations print, in this machine's language and in English, and the free
/// space reading covers everything it misses.
fn disk_full(failure: &str) -> bool {
    const MARKS: [&str; 8] = [
        "no space left on device",
        "not enough space on the disk",
        "disk quota exceeded",
        "database or disk is full",
        "sqlite_full",
        "enospc",
        "errno 28",
        "磁盘空间不足",
    ];
    let lower = failure.to_lowercase();
    MARKS.iter().any(|mark| lower.contains(mark))
}

/// Whether some volume is down to less room than the reserve holds.
fn below_margin(reserve: &Reserve, home: &Path) -> bool {
    volumes(home)
        .iter()
        .any(|volume| volume.free.is_some_and(|free| free < reserve.bytes()))
}

/// The volumes this server could lose room on: every fixed drive, and the home's
/// own volume even when it is neither.
///
/// # Invariant
///
/// The home's volume is the one that matters -- the database, the log, spilled
/// tool output and job records are all there -- and the rest are reported so a
/// monitor starts with the map instead of spending its first tool call on `df`.
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

/// Outside Windows a mount table is a dependency this does not need: the home's
/// own volume is the one this server cannot do without.
#[cfg(not(windows))]
fn fixed_volumes() -> Vec<Volume> {
    Vec::new()
}

/// The volume a path stands on, by the name that identifies it.
#[cfg(windows)]
fn volume_of(path: &Path) -> Option<Volume> {
    // A path's volume is its drive letter here: no `GetVolumePathName`, so a
    // mounted directory is read as its host drive, which is where the room is.
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
