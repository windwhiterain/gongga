//! One home directory, one owner, claimed by a pid file.
//!
//! See `docs/notes/home-lock.md`.

use std::fs::OpenOptions;
use std::io::{ErrorKind, Write};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use anyhow::Context;

/// How long a lock that is being written is given before it is called broken.
const CLAIM_GRACE: Duration = Duration::from_millis(250);

/// How often the lock is re-read while waiting out a claim.
const CLAIM_POLL: Duration = Duration::from_millis(10);

/// How many passes the claim makes before giving up on a home that keeps changing.
const ATTEMPTS: usize = 3;

/// Claims one home directory for this process. The claim lasts as long as the value.
#[derive(Debug)]
pub struct HomeLock {
    path: PathBuf,
    pid: u32,
}

/// What a lock file says right now.
#[derive(Debug, PartialEq, Eq)]
enum Holder {
    /// It names this pid.
    Pid(u32),
    /// It exists but has no pid in it yet: a claim being written.
    Reading,
    /// There is no lock file.
    Missing,
}

impl HomeLock {
    /// `file` names the lock; `owner` is how a taken home is reported.
    ///
    /// # Invariant
    ///
    /// The home is created when missing; an unwritable one refuses before use.
    pub fn acquire(home: &Path, file: &str, owner: &str) -> anyhow::Result<Self> {
        std::fs::create_dir_all(home).with_context(|| format!("creating {}", home.display()))?;
        let path = home.join(file);
        let pid = std::process::id();

        for _ in 0..ATTEMPTS {
            if claim(&path, pid)? {
                return Ok(Self { path, pid });
            }

            // Someone was here first -- or a crash left its pid behind.
            let holder = match read_holder(&path) {
                Holder::Reading => wait_out_claim(&path),
                other => other,
            };
            match holder {
                // A pid nobody holds any more is litter, not a lock: its process was killed.
                Holder::Pid(holder) if alive(holder) => {
                    anyhow::bail!(
                        "another {owner} is already using this home \
                         (pid {holder}, home {}); stop it first, pass another directory, \
                         or delete {} if you are sure that process is gone",
                        home.display(),
                        path.display()
                    );
                }
                Holder::Pid(_) => take_over(&path)?,
                Holder::Reading => {
                    anyhow::bail!(
                        "another {owner} is starting on this home (home {}); {} exists \
                         but has no pid in it yet. If nothing is running, delete it",
                        home.display(),
                        path.display()
                    );
                }
                // It went away between creating and reading: go round and race
                // for it again.
                Holder::Missing => {}
            }
        }
        anyhow::bail!(
            "could not claim {}: it kept changing hands; try again",
            path.display()
        )
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn pid(&self) -> u32 {
        self.pid
    }
}

impl Drop for HomeLock {
    fn drop(&mut self) {
        // Remove only our own claim: a lock naming someone else is theirs.
        if read_holder(&self.path) == Holder::Pid(self.pid) {
            // Best effort: a kill leaves the file behind, which is exactly what
            // the liveness check above exists for.
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

/// Creates `path` and writes `pid` through that same handle; false if taken.
fn claim(path: &Path, pid: u32) -> anyhow::Result<bool> {
    match OpenOptions::new().write(true).create_new(true).open(path) {
        Ok(mut file) => {
            // Written through this handle and synced: the file is visible the moment
            // it exists.
            file.write_all(pid.to_string().as_bytes())
                .and_then(|()| file.sync_all())
                .with_context(|| format!("writing {}", path.display()))?;
            Ok(true)
        }
        Err(e) if e.kind() == ErrorKind::AlreadyExists => Ok(false),
        Err(e) => Err(e).with_context(|| format!("creating {}", path.display())),
    }
}

/// Reads what the lock says, without guessing.
fn read_holder(path: &Path) -> Holder {
    match std::fs::read_to_string(path) {
        Ok(text) => match text.trim().parse::<u32>() {
            // `0` is not a pid; a half-written file reads that way, so it is no holder.
            Ok(pid) if pid != 0 => Holder::Pid(pid),
            _ => Holder::Reading,
        },
        Err(e) if e.kind() == ErrorKind::NotFound => Holder::Missing,
        // Unreadable otherwise: assume a writer, the direction that cannot split a home.
        Err(_) => Holder::Reading,
    }
}

/// Waits out a claim that is being written, then reports what it says.
///
/// # Invariant
///
/// An empty lock is waited out and then refused, never taken over: taking one
/// over is how two owners end up sharing a home.
fn wait_out_claim(path: &Path) -> Holder {
    let deadline = Instant::now() + CLAIM_GRACE;
    loop {
        std::thread::sleep(CLAIM_POLL);
        match read_holder(path) {
            Holder::Reading if Instant::now() < deadline => continue,
            other => return other,
        }
    }
}

/// Moves a stale lock out of the way so the next `create_new` can happen.
///
/// # Invariant
///
/// `rename` is the atomic step: of two processes that both judged the holder
/// dead, exactly one moves the file, and only one `create_new` then wins it.
fn take_over(path: &Path) -> anyhow::Result<()> {
    let aside = path.with_file_name(format!(
        "{}.stale-{}-{}",
        path.file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default(),
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    match std::fs::rename(path, &aside) {
        Ok(()) => {
            let _ = std::fs::remove_file(&aside);
            Ok(())
        }
        // Somebody else moved or removed it first: the caller's next attempt
        // finds out who owns the home now.
        Err(e) if e.kind() == ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e).with_context(|| format!("taking over {}", path.display())),
    }
}

/// Whether a pid is a live process, asked the way each platform allows
/// without pulling in a dependency for it.
fn alive(pid: u32) -> bool {
    #[cfg(windows)]
    {
        // `tasklist` prints the matching line or nothing; the pid match is locale-free.
        let Ok(out) = Command::new("tasklist")
            .args(["/NH", "/FI", &format!("PID eq {pid}")])
            .output()
        else {
            // Cannot tell: assume the holder is alive rather than let two
            // owners into the same home on a hunch.
            return true;
        };
        String::from_utf8_lossy(&out.stdout).contains(&pid.to_string())
    }
    #[cfg(not(windows))]
    {
        Command::new("kill")
            .args(["-0", &pid.to_string()])
            .status()
            .is_ok_and(|s| s.success())
    }
}
