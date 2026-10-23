//! One server per home directory, claimed by a `server.lock`.
//!
//! See `docs/notes/server-instance.md`.

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
                // A pid nobody holds any more is litter, not a lock: its process was killed.
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
        // Remove only our own claim: a lock naming someone else is theirs.
        if read_holder(&self.path) == Holder::Pid(self.pid) {
            // Best effort: a kill leaves the file behind, which is exactly what
            // the liveness check above exists for.
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

/// Creates `path` and writes `pid` through that same handle; false if taken.
///
/// See `docs/notes/server-instance.md`.
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
/// over is how two servers end up sharing a home.
///
/// See `docs/notes/server-instance.md`.
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
/// `rename` is the atomic step: of two servers that both judged the holder dead,
/// exactly one moves the file, and only one `create_new` then wins it.
///
/// See `docs/notes/server-instance.md`.
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
        // `tasklist` prints the matching line or nothing; the pid match is locale-free.
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

    /// A lock with no pid in it is a claim being written, not litter.
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

    /// Dropping a claim removes only that claim; a new owner's lock must survive.
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
        // Winners hold their claim until every thread is done, or the test proves nothing.
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
