//! One checkout, kept as official plus personal.
//!
//! See `docs/notes/server-fork.md`.

use std::path::{Path, PathBuf};
use std::process::Command;

/// The branch a repository publishes on.
pub const OFFICIAL: &str = "main";

/// The machine's own edits. No upstream: it moves only by rebase.
pub const PERSONAL: &str = "local";

/// What one refresh did, for the line it writes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Sync {
    /// There was nothing here; a clone was made from this URL.
    Cloned(String),
    /// The checkout moved to a newer commit.
    Fetched,
    /// Already at what the remote offers, and the personal branch is on top.
    Current,
    /// The directory exists and is not a clone: left exactly as it is.
    Foreign,
    /// `local` conflicts with upstream: the rebase was undone and `local` kept.
    Conflicted,
}

/// A repository the machine reads and sometimes edits.
#[derive(Debug, Clone)]
pub struct Fork {
    dir: PathBuf,
}

impl Fork {
    pub fn at(dir: &Path) -> Self {
        Self {
            dir: dir.to_path_buf(),
        }
    }

    /// Where the checkout is.
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Whether there is a clone here at all.
    pub fn exists(&self) -> bool {
        self.dir.join(".git").exists()
    }

    /// Clones it when it is absent, and leaves anything else alone.
    ///
    /// # Invariant
    ///
    /// A directory this module did not clone is never written to: the roots it
    /// serves are read by the server and owned by whoever put them there.
    pub fn ensure(&self, urls: &[String]) -> anyhow::Result<Sync> {
        if self.exists() {
            return Ok(Sync::Current);
        }
        if present(&self.dir) {
            return Ok(Sync::Foreign);
        }
        self.clone(urls)
    }

    /// Makes the checkout match its remote, keeping the personal branch on top.
    ///
    /// # Invariant
    ///
    /// Best effort by construction: a machine with no route to the repository keeps
    /// what it has. Nothing here is refused because a network was silent, and the
    /// caller decides what an unreadable root means for it.
    pub fn sync(&self) -> anyhow::Result<Sync> {
        if !self.exists() {
            anyhow::bail!("{} is not a clone", self.dir.display());
        }
        let before = self.head();
        // Not `pull`: the personal branch has no upstream to pull from.
        git_ok(&self.dir, &["fetch", "-q", "origin"])?;
        let official = self.fast_forward_official()?;
        let rebased = self.rebase_personal(&official);
        if let Some(official) = official
            && let Rebasing::Conflicted(why) = &rebased
        {
            // An error, not a warning: the checkout is left behind, and every later
            // start reads the content it was left at. The words are git's own, because
            // "conflicts" and "a dirty tree refused to move" are not the same fault.
            tracing::error!(
                fork = %self.dir.display(),
                "the personal branch was not rebased: {why}; the commits to rebase are \
                 in `git -C {} log --oneline {official}..{PERSONAL}`",
                self.dir.display()
            );
        }
        Ok(match rebased {
            Rebasing::Conflicted(_) => Sync::Conflicted,
            _ if before == self.head() => Sync::Current,
            _ => Sync::Fetched,
        })
    }

    /// The commit this checkout's content is: the personal branch when there is
    /// one, and the official branch otherwise.
    pub fn content(&self) -> Option<String> {
        self.branch_commit(PERSONAL)
            .or_else(|| self.branch_commit(OFFICIAL))
            .or_else(|| self.head())
    }

    /// The commit the official branch is at here, after a fetch.
    pub fn official(&self) -> Option<String> {
        self.branch_commit(OFFICIAL)
    }

    /// The commit a branch names, if that branch exists.
    pub fn branch_commit(&self, name: &str) -> Option<String> {
        let out = git(
            &self.dir,
            &["rev-parse", "--verify", "-q", &format!("refs/heads/{name}")],
        );
        out.status
            .success()
            .then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
    }

    /// The branch checked out, or `None` when HEAD is detached.
    pub fn branch(&self) -> Option<String> {
        let out = git(&self.dir, &["symbolic-ref", "--quiet", "--short", "HEAD"]);
        if !out.status.success() {
            return None;
        }
        let name = String::from_utf8_lossy(&out.stdout).trim().to_string();
        (!name.is_empty()).then_some(name)
    }

    /// The commit this checkout is on.
    pub fn head(&self) -> Option<String> {
        let out = git(&self.dir, &["rev-parse", "HEAD"]);
        out.status
            .success()
            .then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
    }

    /// One file as a commit has it, which is how content that is not checked out is
    /// read.
    pub fn blob(&self, commit: &str, path: &str) -> Option<Vec<u8>> {
        let out = git(&self.dir, &["show", &format!("{commit}:{path}")]);
        out.status.success().then_some(out.stdout)
    }

    /// The first URL that clones, or why none of them did.
    fn clone(&self, urls: &[String]) -> anyhow::Result<Sync> {
        if let Some(parent) = self.dir.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut last = "no clone URL was configured".to_string();
        for url in urls {
            let out = Command::new(program())
                .args(["clone", "-q"])
                .arg(url)
                .arg(&self.dir)
                .output()?;
            if out.status.success() {
                return Ok(Sync::Cloned(url.clone()));
            }
            last = format!("{url}: {}", String::from_utf8_lossy(&out.stderr).trim());
            // A half-made tree makes the next URL refuse to clone into it.
            let _ = std::fs::remove_dir_all(&self.dir);
        }
        anyhow::bail!("cannot clone {} ({last})", self.dir.display())
    }

    /// Moves the official branch to the remote's, out of the worktree's way.
    fn fast_forward_official(&self) -> anyhow::Result<Option<String>> {
        let remote = format!("refs/remotes/origin/{OFFICIAL}");
        if !git(&self.dir, &["rev-parse", "--verify", "-q", &remote])
            .status
            .success()
        {
            return Ok(None);
        }
        if self.branch().as_deref() == Some(OFFICIAL) {
            // A checked-out branch cannot be moved by a refspec.
            let out = git(&self.dir, &["merge", "--ff-only", "-q", &remote]);
            if !out.status.success() {
                tracing::warn!(
                    fork = %self.dir.display(),
                    said = %String::from_utf8_lossy(&out.stderr).trim(),
                    "the official branch did not fast-forward"
                );
            }
        } else {
            let spec = format!("{remote}:refs/heads/{OFFICIAL}");
            let out = git(&self.dir, &["fetch", "-q", ".", &spec]);
            if !out.status.success() {
                // Somebody committed locally: leave it rather than overwrite.
                tracing::warn!(
                    fork = %self.dir.display(),
                    said = %String::from_utf8_lossy(&out.stderr).trim(),
                    "the local official branch did not fast-forward"
                );
            }
        }
        Ok(self.official())
    }

    /// Puts the personal branch back on top of `base`.
    ///
    /// # Invariant
    ///
    /// A conflict is undone, not left behind: a half-finished rebase would block the
    /// next refresh and confuse anyone typing `git status`. The branch keeps the
    /// commits it had, the conflict is reported, and the next refresh tries again --
    /// a person resolves it when they are ready, never the server.
    fn rebase_personal(&self, base: &Option<String>) -> Rebasing {
        let Some(base) = base else {
            return Rebasing::Settled;
        };
        if self.branch_commit(PERSONAL).is_none() {
            return Rebasing::Settled;
        }
        // Rebasing moves the worktree: only when that branch is the one out.
        if self.branch().as_deref() != Some(PERSONAL) {
            return Rebasing::Settled;
        }
        if git(&self.dir, &["merge-base", "--is-ancestor", base, PERSONAL])
            .status
            .success()
        {
            return Rebasing::Settled;
        }
        // No `--autostash`: a checkout holding work is the reader's, and moving it is
        // not this function's to do. The refusal is reported, with git's own words.
        let rebase = git(&self.dir, &["rebase", "-q", base]);
        if rebase.status.success() {
            return Rebasing::Rebased;
        }
        let why = String::from_utf8_lossy(&rebase.stderr)
            .lines()
            .find(|line| !line.trim().is_empty())
            .unwrap_or("git said nothing")
            .trim()
            .to_string();
        let _ = git(&self.dir, &["rebase", "--abort"]);
        Rebasing::Conflicted(why)
    }
}

/// What happened to the personal branch.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Rebasing {
    /// Nothing to do: no personal branch, or it is already on top.
    Settled,
    /// Moved onto the official branch.
    Rebased,
    /// Refused, and undone: git's first line of complaint, in git's own words.
    Conflicted(String),
}

/// Whether anything at all is at `dir`.
fn present(dir: &Path) -> bool {
    std::fs::read_dir(dir)
        .map(|mut entries| entries.next().is_some())
        .unwrap_or(false)
}

/// `git` by name: it is what the machine has, and a path resolved at build time is
/// wrong on the next one.
pub fn program() -> &'static str {
    "git"
}

/// Git bounded by silence, not duration: silence is what hangs a start.
pub fn bounded() -> Command {
    let mut cmd = Command::new(program());
    cmd.env("GIT_TERMINAL_PROMPT", "0")
        .env(
            "GIT_SSH_COMMAND",
            format!("{} -o BatchMode=yes -o ConnectTimeout=10", ssh()),
        )
        .args([
            "-c",
            "http.lowSpeedLimit=1000",
            "-c",
            "http.lowSpeedTime=20",
        ]);
    cmd
}

/// OpenSSH, which a `git@` clone URL goes through.
fn ssh() -> &'static str {
    "ssh"
}

/// `git -C <dir>`, so one checkout is asked about instead of the process's directory.
pub fn git(dir: &Path, args: &[&str]) -> std::process::Output {
    bounded()
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .unwrap_or_else(|e| panic!("running git {args:?}: {e}"))
}

/// The same, as an error when git refused.
pub fn git_ok(dir: &Path, args: &[&str]) -> anyhow::Result<()> {
    let out = git(dir, args);
    anyhow::ensure!(
        out.status.success(),
        "git {} in {}: {}",
        args.join(" "),
        dir.display(),
        String::from_utf8_lossy(&out.stderr).trim()
    );
    Ok(())
}
