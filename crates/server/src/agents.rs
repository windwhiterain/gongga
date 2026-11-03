//! The machine's shared content: `~/.agents/gongga-agents`.
//!
//! See `docs/notes/server-agents.md`.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use crate::fork::{Fork, OFFICIAL, PERSONAL, Sync};

/// The roster file, named the same under both roots.
pub const ROSTER: &str = "providers.toml";

/// Where the community content comes from, in the order tried: https, then ssh.
const URLS: [&str; 2] = [
    "https://github.com/windwhiterain/gongga-agents.git",
    "git@github.com:windwhiterain/gongga-agents.git",
];

/// How often a running server refreshes the clone.
const REFRESH: Duration = Duration::from_secs(60 * 60);

/// The directory community skills are read through, named the same under both roots.
const SKILLS: &str = "skills";

/// The two roots a roster is assembled from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Roots {
    /// The community clone; this module is the only writer.
    pub community: PathBuf,
    /// The user's own `~/.agents`, which is never written to.
    pub private: PathBuf,
}

impl Roots {
    /// What this machine has: the env overrides, else `~/.agents`.
    pub fn of() -> Self {
        let private = std::env::var("GONGGA_AGENTS_HOME")
            .ok()
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
            .unwrap_or_else(|| home().join(".agents"));
        let community = std::env::var("GONGGA_AGENTS_DIR")
            .ok()
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
            .unwrap_or_else(|| private.join("gongga-agents"));
        Self { community, private }
    }

    /// The one roster this machine reads: the community checkout.
    ///
    /// # Invariant
    ///
    /// One file, not two. A machine's own entries live on its own branch of that
    /// checkout (`local`), so an upstream change is a rebase rather than an overlay
    /// this server would have to merge -- and a merge is the thing that drifts.
    pub fn roster(&self) -> PathBuf {
        self.community.join(ROSTER)
    }
}

/// The home directory, or the working directory when there is none.
fn home() -> PathBuf {
    dirs::home_dir().unwrap_or_else(|| PathBuf::from("."))
}

/// Links every community skill into the machine's own `skills/`, and prunes the gone.
///
/// # Invariant
///
/// A name the reader already has is left untouched -- that directory is the
/// override -- and a skill is only linked when it holds a `SKILL.md`. A link
/// whose target went away is removed, so the index lists what exists.
pub fn link_skills(roots: &Roots) -> usize {
    let from = roots.community.join(SKILLS);
    let into = roots.private.join(SKILLS);
    let Ok(entries) = std::fs::read_dir(&from) else {
        return 0;
    };
    if std::fs::create_dir_all(&into).is_err() {
        return 0;
    }
    let mut linked = 0;
    for entry in entries.flatten() {
        let source = entry.path();
        let Some(name) = source.file_name() else {
            continue;
        };
        if !source.is_dir() || !source.join("SKILL.md").is_file() {
            continue;
        }
        // A dangling entry is a skill that left the clone: drop it before linking.
        let at = into.join(name);
        if std::fs::symlink_metadata(&at).is_ok() && !at.exists() {
            let _ = std::fs::remove_dir(&at);
            let _ = std::fs::remove_file(&at);
        }
        if std::fs::symlink_metadata(&at).is_ok() {
            continue;
        }
        match link_dir(&source, &at) {
            Ok(()) => linked += 1,
            // Worth a line: a machine that cannot link needs to hear it once.
            Err(e) => tracing::warn!(
                skill = %at.display(),
                error = %e,
                "cannot link a community skill"
            ),
        }
    }
    prune(&into);
    linked
}

/// Removes the links whose skill is no longer in the clone.
fn prune(into: &Path) {
    let Ok(entries) = std::fs::read_dir(into) else {
        return;
    };
    for entry in entries.flatten() {
        let at = entry.path();
        if std::fs::symlink_metadata(&at).is_ok() && !at.exists() {
            let _ = std::fs::remove_dir(&at);
            let _ = std::fs::remove_file(&at);
        }
    }
}

#[cfg(unix)]
fn link_dir(target: &Path, at: &Path) -> std::io::Result<()> {
    std::os::unix::fs::symlink(target, at)
}

/// A junction: a directory symlink wants a privilege the server does not hold.
#[cfg(windows)]
fn link_dir(target: &Path, at: &Path) -> std::io::Result<()> {
    if std::os::windows::fs::symlink_dir(target, at).is_ok() {
        return Ok(());
    }
    let out = Command::new("cmd")
        .args(["/C", "mklink", "/J"])
        .arg(at)
        .arg(target)
        .output()?;
    if out.status.success() {
        Ok(())
    } else {
        Err(std::io::Error::other(
            String::from_utf8_lossy(&out.stdout).trim().to_string(),
        ))
    }
}

/// Makes the community root match its remote, cloning it when it is absent.
///
/// # Invariant
///
/// Best effort: a machine with no route to the repository keeps whatever it has,
/// and a directory this module did not clone is never written to, because gongga
/// reads the community root and must not own what a reader put there. The
/// fetch-ff-rebase rule itself is `fork`'s: the shipped content is kept the same
/// way, so there is one answer to what a refresh means.
pub fn sync(roots: &Roots, urls: &[String]) -> anyhow::Result<Sync> {
    let fork = Fork::at(&roots.community);
    match fork.ensure(urls)? {
        // A fresh clone is at the remote's tip already, and the personal branch
        // came with it.
        done @ Sync::Cloned(_) => Ok(done),
        Sync::Foreign => Ok(Sync::Foreign),
        _ => {
            let outcome = fork.sync()?;
            if outcome == Sync::Conflicted {
                tracing::warn!(
                    community = %fork.dir().display(),
                    "the machine's branch conflicts with upstream; see `git -C {} log --oneline \
                     {}..{PERSONAL}`",
                    fork.dir().display(),
                    OFFICIAL
                );
            }
            Ok(match outcome {
                Sync::Conflicted | Sync::Current => Sync::Current,
                other => other,
            })
        }
    }
}

/// Refreshes the community root once, then hourly, off the caller's thread.
///
/// # Invariant
///
/// Never blocks and never refuses to return: the roster falls back to what is on
/// disk, so no read may wait for a network, and a failed refresh is one warning.
pub fn start() {
    static STARTED: AtomicBool = AtomicBool::new(false);
    if STARTED.swap(true, Ordering::SeqCst) {
        return;
    }
    let roots = Roots::of();
    tokio::spawn(async move {
        loop {
            let dir = roots.clone();
            let synced = tokio::task::spawn_blocking(move || {
                let outcome = sync(&dir, &urls());
                (outcome, link_skills(&dir))
            })
            .await;
            match synced {
                Ok((Ok(sync), linked)) => tracing::info!(
                    community = %roots.community.display(),
                    ?sync,
                    linked,
                    "community content"
                ),
                Ok((Err(e), linked)) => tracing::warn!(
                    community = %roots.community.display(),
                    linked,
                    error = %e,
                    "cannot refresh the community content"
                ),
                Err(e) => tracing::warn!(error = %e, "the community refresh task ended"),
            }
            tokio::time::sleep(REFRESH).await;
        }
    });
}

/// The clone URLs to try: `GONGGA_AGENTS_URL` (comma separated) over the built-in pair.
fn urls() -> Vec<String> {
    std::env::var("GONGGA_AGENTS_URL")
        .ok()
        .filter(|v| !v.is_empty())
        .map(|v| {
            v.split(',')
                .map(str::trim)
                .filter(|u| !u.is_empty())
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_else(|| URLS.iter().map(|u| u.to_string()).collect())
}
