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

use std::fs::OpenOptions;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::Context;

/// Claims `home` for this process. The claim lasts as long as the value does.
#[derive(Debug)]
pub struct InstanceLock {
    path: PathBuf,
}

impl InstanceLock {
    pub fn acquire(home: &Path) -> anyhow::Result<Self> {
        let path = home.join("server.lock");
        let pid = std::process::id();

        match OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(_) => {
                std::fs::write(&path, pid.to_string())
                    .with_context(|| format!("writing {}", path.display()))?;
                return Ok(Self { path });
            }
            // Someone was here first -- or a crash left its pid behind.
            Err(e) if e.kind() == ErrorKind::AlreadyExists => {}
            Err(e) => {
                return Err(e).with_context(|| format!("creating {}", path.display()));
            }
        }

        // A pid nobody holds any more is not a lock, it is litter: the process
        // that wrote it was killed before it could clean up.
        let held = std::fs::read_to_string(&path).unwrap_or_default();
        let holder: u32 = held.trim().parse().unwrap_or(0);
        if holder != 0 && alive(holder) {
            anyhow::bail!(
                "another gongga server is already running on this home \
                 (pid {holder}, home {}); stop it first, or delete {} \
                 if you are sure that process is gone",
                home.display(),
                path.display()
            );
        }
        std::fs::write(&path, pid.to_string())
            .with_context(|| format!("writing {}", path.display()))?;
        Ok(Self { path })
    }
}

impl Drop for InstanceLock {
    fn drop(&mut self) {
        // Best effort: a kill leaves the file behind, which is exactly what
        // the liveness check above exists for.
        let _ = std::fs::remove_file(&self.path);
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
}
