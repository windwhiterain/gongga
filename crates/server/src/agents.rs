//! The machine's shared content: `~/.agents/gongga-agents`.
//!
//! See `docs/notes/server-agents.md`.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

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

/// What a refresh found, for the one line it writes.
#[derive(Debug, PartialEq, Eq)]
pub enum Sync {
    /// There was no clone here; one was made from this URL.
    Cloned(String),
    /// The clone moved to a newer commit.
    Fetched,
    /// Already at what the remote offers.
    Current,
    /// The directory exists and is not a clone: left exactly as it is.
    Foreign,
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

/// A git command that gives up on a remote that stopped answering.
///
/// # Invariant
///
/// The refresh runs on the blocking pool, which the exit joins, so a stalled
/// transfer is a stop that does not stop. Git's own bound does the work: below a
/// trickle for longer than the limit it aborts, and a credential prompt -- which
/// has no console to appear on -- is refused. Silence is what is bounded, never
/// duration: a slow transfer is still allowed to finish.
fn bounded(git: &str) -> Command {
    let mut cmd = Command::new(git);
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

/// OpenSSH, which a `git@` clone URL goes through; a fake git in a test ignores it.
fn ssh() -> &'static str {
    "ssh"
}

/// Makes the community root match its remote, cloning it when it is absent.
///
/// # Invariant
///
/// Best effort: a machine with no route to the repository keeps whatever it has,
/// and a directory this module did not clone is never written to, because gongga
/// reads the community root and must not own what a reader put there.
pub fn sync(roots: &Roots, urls: &[String]) -> anyhow::Result<Sync> {
    let dir = &roots.community;
    if !dir.join(".git").exists() {
        if present(dir) {
            return Ok(Sync::Foreign);
        }
        return clone(dir, urls);
    }
    let before = head(dir);
    // `fetch`, then fast-forward `main`, then put the machine's own branch back on
    // top of it. Not `pull`: that needs the current branch to name an upstream, and
    // the branch this machine reads (`local`) deliberately has none -- its commits
    // are not meant to be pushed anywhere.
    git_ok(dir, &["fetch", "-q", "origin"])?;
    let main = fast_forward_main(dir)?;
    let rebased = rebase_local(dir, &main)?;
    if rebased == Rebasing::Conflicted {
        tracing::warn!(
            community = %dir.display(),
            "the machine's branch conflicts with upstream; left as it was, and the \
             commits to rebase are in `git -C {} log --oneline {}..local`",
            dir.display(),
            main
        );
    }
    Ok(if before == head(dir) {
        Sync::Current
    } else {
        Sync::Fetched
    })
}

/// What happened to the branch this machine keeps on top of upstream.
#[derive(Debug, PartialEq, Eq)]
enum Rebasing {
    /// Nothing to do: no such branch, or it is already on top.
    Settled,
    /// Moved onto upstream.
    Rebased,
    /// Refused: a rebase was attempted, hit a conflict, and was undone.
    Conflicted,
}

/// Fast-forwards the local `main` to what `origin/main` now is, without checking
/// it out: this server must not move the worktree a person may be looking at.
fn fast_forward_main(dir: &Path) -> anyhow::Result<String> {
    // `fetch . origin/main:main` moves the ref without touching the worktree, which
    // is what a background refresh must do: a person may be reading it. A branch
    // that is checked out cannot be moved this way, so a worktree already on `main`
    // needs the ordinary fast-forward instead.
    if branch(dir).as_deref() == Some("main") {
        let _ = git(dir, &["merge", "--ff-only", "-q", "origin/main"]);
    } else if git(dir, &["rev-parse", "--verify", "-q", "origin/main"])
        .status
        .success()
    {
        let out = git(
            dir,
            &[
                "fetch",
                "-q",
                ".",
                "refs/remotes/origin/main:refs/heads/main",
            ],
        );
        if !out.status.success() {
            // A non-fast-forward means this machine committed to its own main.
            // Leave it: overwriting a person's branch is worse than a stale one.
            tracing::warn!(
                community = %dir.display(),
                said = %String::from_utf8_lossy(&out.stderr).trim(),
                "local main did not fast-forward"
            );
        }
    }
    let rev = git(dir, &["rev-parse", "main"]);
    if rev.status.success() {
        return Ok(String::from_utf8_lossy(&rev.stdout).trim().to_string());
    }
    let rev = git(dir, &["rev-parse", "origin/main"]);
    anyhow::ensure!(
        rev.status.success(),
        "no main and no origin/main in {}",
        dir.display()
    );
    Ok(String::from_utf8_lossy(&rev.stdout).trim().to_string())
}

/// Puts the machine's branch (`local`) back on top of `base`.
///
/// # Invariant
///
/// A conflict is undone, not left behind: a half-finished rebase would block the
/// next refresh and confuse anyone typing `git status`. The branch keeps the
/// commits it had, the conflict is reported, and the next refresh tries again --
/// a person resolves it when they are ready, never the server.
fn rebase_local(dir: &Path, base: &str) -> anyhow::Result<Rebasing> {
    if !has_branch(dir, BRANCH) {
        return Ok(Rebasing::Settled);
    }
    // Rebasing moves the worktree, so only do it when that branch is the one
    // checked out: a background refresh must not switch what a window is showing.
    if branch(dir).as_deref() != Some(BRANCH) {
        return Ok(Rebasing::Settled);
    }
    if git(dir, &["merge-base", "--is-ancestor", base, BRANCH])
        .status
        .success()
    {
        return Ok(Rebasing::Settled);
    }
    if git(dir, &["rebase", "-q", base]).status.success() {
        return Ok(Rebasing::Rebased);
    }
    let _ = git(dir, &["rebase", "--abort"]);
    Ok(Rebasing::Conflicted)
}

fn has_branch(dir: &Path, name: &str) -> bool {
    git(
        dir,
        &["rev-parse", "--verify", "-q", &format!("refs/heads/{name}")],
    )
    .status
    .success()
}

/// The branch the worktree has checked out, or `None` when HEAD is detached.
fn branch(dir: &Path) -> Option<String> {
    let out = git(dir, &["symbolic-ref", "--quiet", "--short", "HEAD"]);
    if !out.status.success() {
        return None;
    }
    let name = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (!name.is_empty()).then_some(name)
}

fn git(dir: &Path, args: &[&str]) -> std::process::Output {
    bounded(crate::ship::git())
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .unwrap_or_else(|e| panic!("running git {args:?}: {e}"))
}

fn git_ok(dir: &Path, args: &[&str]) -> anyhow::Result<()> {
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

/// The branch a machine keeps its own entries on. See `LOCAL.md` in that repository.
const BRANCH: &str = "local";

/// The first URL that clones, or why none of them did.
fn clone(dir: &Path, urls: &[String]) -> anyhow::Result<Sync> {
    if let Some(parent) = dir.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut last = "no clone URL was configured".to_string();
    for url in urls {
        let out = bounded(crate::ship::git())
            .args(["clone", "-q"])
            .arg(url)
            .arg(dir)
            .output()?;
        if out.status.success() {
            return Ok(Sync::Cloned(url.clone()));
        }
        last = format!("{url}: {}", String::from_utf8_lossy(&out.stderr).trim());
        // A half-made tree makes the next URL refuse to clone into it.
        let _ = std::fs::remove_dir_all(dir);
    }
    anyhow::bail!("cannot clone the community content ({last})")
}

/// Whether anything at all is at `dir`.
fn present(dir: &Path) -> bool {
    std::fs::read_dir(dir)
        .map(|mut entries| entries.next().is_some())
        .unwrap_or(false)
}

/// The commit the clone is on, or nothing when git cannot say.
fn head(dir: &Path) -> Option<String> {
    let out = Command::new(crate::ship::git())
        .arg("-C")
        .arg(dir)
        .args(["rev-parse", "HEAD"])
        .output()
        .ok()
        .filter(|out| out.status.success())?;
    Some(String::from_utf8_lossy(&out.stdout).trim().to_string())
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
