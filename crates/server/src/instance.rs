//! One server per home directory.
//!
//! Two servers sharing one home is a state nothing here has an answer for:
//! they share the SQLite file, so each would resume the same pending turn,
//! compress the same transcript, and drive the same session at the same time.
//! Binding the UDP port already refuses a second server on the *same* port,
//! but a different port pointed at the same home slips past that, and both
//! servers would then happily run the same agent twice.
//!
//! So the home itself is claimed: a `server.lock` holding this process's pid.
//! `create_new` is atomic, so of two servers starting together exactly one
//! wins, and the loser is told who holds it. A pid left behind by a kill is
//! recognised as stale and taken over, so a crash does not lock the home
//! forever -- only a live process does.
//!
//! The pid is written through the handle `create_new` returned, never by a
//! separate `fs::write` afterwards: the file becomes visible the moment it is
//! created, so a second server arriving in between would read an empty lock,
//! call it litter, and take a home that was already claimed. For the same
//! reason an empty or unparsable lock is *not* stale: it is a claim being
//! written, and it is waited out (briefly) rather than taken over.

use std::fs::OpenOptions;
use std::io::{ErrorKind, Write};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use anyhow::Context;

/// How long a lock that is being written is given before it is called broken.
///
/// The writer writes one short line through an already-open handle, so this is
/// orders of magnitude more than it needs; it exists so a genuinely empty file
/// (a disk that lies, a killed `create_new`) fails with a message instead of
/// waiting forever.
const CLAIM_GRACE: Duration = Duration::from_millis(250);

/// How often the lock is re-read while waiting out a claim.
const CLAIM_POLL: Duration = Duration::from_millis(10);

/// How many times the whole claim is retried.
///
/// Each pass either wins `create_new` or learns something new about the file --
/// it disappeared, it is being written, it names a live pid. The loop exists
/// because those are separate observations of a file other processes are
/// changing: a pass that finds the file gone goes round once more and races for
/// it.
const ATTEMPTS: usize = 3;

/// Claims `home` for this process. The claim lasts as long as the value does.
#[derive(Debug)]
pub struct InstanceLock {
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

impl InstanceLock {
    pub fn acquire(home: &Path) -> anyhow::Result<Self> {
        let path = home.join("server.lock");
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
                // A pid nobody holds any more is not a lock, it is litter: the
                // process that wrote it was killed before it could clean up.
                Holder::Pid(holder) if alive(holder) => {
                    anyhow::bail!(
                        "another gongga server is already running on this home \
                         (pid {holder}, home {}); stop it first, or delete {} \
                         if you are sure that process is gone",
                        home.display(),
                        path.display()
                    );
                }
                Holder::Pid(_) => take_over(&path)?,
                Holder::Reading => {
                    anyhow::bail!(
                        "another gongga server is starting on this home (home {}); {} exists \
                         but has no pid in it yet. If no server is running, delete it",
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
}

impl Drop for InstanceLock {
    fn drop(&mut self) {
        // Only our own claim is removed. A file that now names somebody else
        // was taken over after a `cancel` outlived this process's own lock --
        // deleting it would hand the home to a third server.
        if read_holder(&self.path) == Holder::Pid(self.pid) {
            // Best effort: a kill leaves the file behind, which is exactly what
            // the liveness check above exists for.
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

/// Creates `path` and writes `pid` into the handle that created it.
///
/// Returns `false` when the file was already there, which is not an error: the
/// caller is the one that knows what to do about the holder.
fn claim(path: &Path, pid: u32) -> anyhow::Result<bool> {
    match OpenOptions::new().write(true).create_new(true).open(path) {
        Ok(mut file) => {
            // Written through *this* handle, and synced before it closes: the
            // file is visible to everybody from the instant `create_new`
            // returns, so its contents must arrive in the same breath.
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
            // `0` is not a pid any process has; it is what a half-written file
            // reads as, which is why it is not treated as a holder either.
            Ok(pid) if pid != 0 => Holder::Pid(pid),
            _ => Holder::Reading,
        },
        Err(e) if e.kind() == ErrorKind::NotFound => Holder::Missing,
        // Unreadable for any other reason: assume somebody is writing it. That
        // is the direction that cannot hand the home to a second server.
        Err(_) => Holder::Reading,
    }
}

/// Waits out a claim that is being written, then reports what it says.
///
/// The writer only has to put a few digits into an open handle, so a lock that
/// stays empty for [`CLAIM_GRACE`] is not being written by anybody -- but that
/// is still not grounds to take it over. The caller refuses instead, and the
/// message names the file so a human can decide.
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
/// `rename` is the atomic part: of two servers that both decided the holder is
/// dead, exactly one moves the file, and the other sees it disappear. Neither
/// writes over the other's claim -- the following `create_new` is what decides
/// the owner, and only one process can win that.
fn take_over(path: &Path) -> anyhow::Result<()> {
    let aside = path.with_file_name(format!(
        "server.lock.stale-{}-{}",
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
        // `tasklist` prints the matching line, or reports that nothing
        // matched. Matching on the pid itself is locale-independent.
        let Ok(out) = Command::new("tasklist")
            .args(["/NH", "/FI", &format!("PID eq {pid}")])
            .output()
        else {
            // Cannot tell: assume the holder is alive rather than let two
            // servers into the same home on a hunch.
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Barrier};

    #[test]
    fn a_second_server_on_the_same_home_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let first = InstanceLock::acquire(dir.path()).unwrap();
        let error = InstanceLock::acquire(dir.path()).unwrap_err();
        assert!(error.to_string().contains("already running"), "{error}");
        // Releasing the claim frees the home again.
        drop(first);
        InstanceLock::acquire(dir.path()).unwrap();
    }

    /// A server that was killed never gets to clean up; its pid must not lock
    /// the home forever.
    #[test]
    fn a_pid_left_behind_by_a_dead_process_is_taken_over() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("server.lock"), "999999").unwrap();
        InstanceLock::acquire(dir.path()).unwrap();
    }

    /// A lock that exists but has no pid in it is a claim being written, not
    /// litter. Taking it over is how two servers end up sharing one home.
    #[test]
    fn an_empty_lock_is_not_taken_over() {
        for contents in ["", "   ", "not-a-pid"] {
            let dir = tempfile::tempdir().unwrap();
            std::fs::write(dir.path().join("server.lock"), contents).unwrap();
            let error = InstanceLock::acquire(dir.path()).unwrap_err();
            assert!(
                error.to_string().contains("starting"),
                "an empty lock was not respected: {error}"
            );
        }
    }

    /// Releasing a claim removes only that claim: a lock that now names a
    /// different process belongs to it, and dropping ours must not delete it.
    #[test]
    fn dropping_a_stale_claim_does_not_delete_the_new_owner() {
        let dir = tempfile::tempdir().unwrap();
        let lock = InstanceLock::acquire(dir.path()).unwrap();
        let path = dir.path().join("server.lock");
        std::fs::write(&path, "4242").unwrap();
        drop(lock);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "4242");
    }

    /// Exactly one of many servers starting together gets the home.
    #[test]
    fn only_one_of_many_starting_together_wins() {
        let dir = tempfile::tempdir().unwrap();
        let home = Arc::new(dir.path().to_path_buf());
        let gate = Arc::new(Barrier::new(8));
        // The winners keep their claim until every thread is done: a claim that
        // is released immediately would let the next thread have the home, and
        // the test would prove nothing.
        let held: Arc<std::sync::Mutex<Vec<InstanceLock>>> = Arc::default();
        let mut threads = Vec::new();
        for _ in 0..8 {
            let home = home.clone();
            let gate = gate.clone();
            let held = held.clone();
            threads.push(std::thread::spawn(move || {
                gate.wait();
                if let Ok(lock) = InstanceLock::acquire(&home) {
                    held.lock().unwrap_or_else(|e| e.into_inner()).push(lock);
                }
            }));
        }
        for thread in threads {
            thread.join().unwrap();
        }
        assert_eq!(
            held.lock().unwrap_or_else(|e| e.into_inner()).len(),
            1,
            "more than one server claimed the same home"
        );
    }
}
