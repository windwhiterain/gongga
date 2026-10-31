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

    /// The roster under each root, lowest precedence first.
    pub fn rosters(&self) -> Vec<PathBuf> {
        vec![self.community.join(ROSTER), self.private.join(ROSTER)]
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
    let out = bounded(crate::ship::git())
        .arg("-C")
        .arg(dir)
        .args(["pull", "--ff-only", "-q"])
        .output()?;
    if !out.status.success() {
        anyhow::bail!(
            "git pull failed in {}: {}",
            dir.display(),
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(if before == head(dir) {
        Sync::Current
    } else {
        Sync::Fetched
    })
}

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

#[cfg(test)]
mod tests {
    use super::*;

    fn git_in(dir: &Path, args: &[&str]) {
        let out = Command::new(crate::ship::git())
            .arg("-C")
            .arg(dir)
            .args(["-c", "user.email=test@gongga", "-c", "user.name=test"])
            .args(args)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    fn roots(community: &Path, private: &Path) -> Roots {
        Roots {
            community: community.to_path_buf(),
            private: private.to_path_buf(),
        }
    }

    /// An origin with one commit, so a clone has something to arrive at.
    fn origin(dir: &Path) {
        std::fs::create_dir_all(dir.join("llm")).unwrap();
        std::fs::write(dir.join("providers.toml"), "[calendar]\n").unwrap();
        std::fs::write(dir.join("llm").join("probe.py"), "print(1)\n").unwrap();
        git_in(dir, &["init", "-q", "-b", "main"]);
        git_in(dir, &["add", "-A"]);
        git_in(dir, &["commit", "-q", "-m", "first"]);
    }

    #[test]
    fn a_missing_root_is_cloned_once_then_kept_current() {
        let d = tempfile::tempdir().unwrap();
        let up = d.path().join("upstream");
        origin(&up);
        let roots = roots(&d.path().join("community"), &d.path().join("private"));
        let urls = vec![up.display().to_string()];

        assert!(matches!(sync(&roots, &urls).unwrap(), Sync::Cloned(_)));
        assert!(roots.community.join("providers.toml").is_file());
        assert!(roots.community.join("llm").join("probe.py").is_file());
        assert_eq!(sync(&roots, &urls).unwrap(), Sync::Current);

        // A commit upstream arrives on the next refresh.
        std::fs::write(up.join("providers.toml"), "[calendar]\nupdated = 1\n").unwrap();
        git_in(&up, &["add", "-A"]);
        git_in(&up, &["commit", "-q", "-m", "second"]);
        assert_eq!(sync(&roots, &urls).unwrap(), Sync::Fetched);
        let refreshed = std::fs::read_to_string(roots.community.join("providers.toml")).unwrap();
        assert!(
            refreshed.contains("updated = 1"),
            "the worktree followed the new commit, CRLF and all: {refreshed:?}"
        );
    }

    /// A directory a reader filled in is theirs: no clone over it, no error either.
    #[test]
    fn a_directory_that_is_not_a_clone_is_left_alone() {
        let d = tempfile::tempdir().unwrap();
        let community = d.path().join("community");
        std::fs::create_dir_all(&community).unwrap();
        std::fs::write(community.join("providers.toml"), "[calendar]\n").unwrap();
        let roots = roots(&community, &d.path().join("private"));
        assert_eq!(
            sync(&roots, &["no-such-url".into()]).unwrap(),
            Sync::Foreign
        );
        assert!(community.join("providers.toml").is_file(), "the file stays");
    }

    #[test]
    fn no_url_that_works_is_an_error_that_names_it() {
        let d = tempfile::tempdir().unwrap();
        let roots = roots(&d.path().join("community"), &d.path().join("private"));
        let err = sync(&roots, &["file:///no/such/repo".into()]).unwrap_err();
        assert!(err.to_string().contains("/no/such/repo"), "{err}");
        assert!(!roots.community.exists(), "a failed clone leaves nothing");
    }

    /// Every community skill shows up in the machine's directory; a name the
    /// reader already has stays theirs.
    #[test]
    fn community_skills_are_linked_and_a_reader_override_stays() {
        let d = tempfile::tempdir().unwrap();
        let community = d.path().join("community");
        let private = d.path().join("private");
        std::fs::create_dir_all(community.join("skills/remote")).unwrap();
        std::fs::write(community.join("skills/remote/SKILL.md"), "theirs\n").unwrap();
        std::fs::create_dir_all(community.join("skills/proxy")).unwrap();
        std::fs::write(community.join("skills/proxy/SKILL.md"), "theirs\n").unwrap();
        std::fs::create_dir_all(private.join("skills/proxy")).unwrap();
        std::fs::write(private.join("skills/proxy/SKILL.md"), "mine\n").unwrap();

        let roots = roots(&community, &private);
        assert_eq!(link_skills(&roots), 1, "only the name nobody has is linked");
        assert_eq!(
            std::fs::read_to_string(private.join("skills/remote/SKILL.md")).unwrap(),
            "theirs\n",
            "readable through the link"
        );
        assert_eq!(
            std::fs::read_to_string(private.join("skills/proxy/SKILL.md")).unwrap(),
            "mine\n",
            "an override is never replaced"
        );
        assert_eq!(link_skills(&roots), 0, "the second pass has nothing to do");
    }

    /// A skill that left the clone loses its link, and nothing else is touched.
    #[test]
    fn a_skill_that_left_the_clone_is_unlinked() {
        let d = tempfile::tempdir().unwrap();
        let community = d.path().join("community");
        let private = d.path().join("private");
        std::fs::create_dir_all(community.join("skills/gone")).unwrap();
        std::fs::write(community.join("skills/gone/SKILL.md"), "x\n").unwrap();
        let roots = roots(&community, &private);
        assert_eq!(link_skills(&roots), 1);

        std::fs::remove_dir_all(community.join("skills/gone")).unwrap();
        assert_eq!(link_skills(&roots), 0);
        assert!(
            std::fs::symlink_metadata(private.join("skills/gone")).is_err(),
            "the dangling link is gone"
        );
    }

    #[test]
    fn the_rosters_are_the_two_roots_in_precedence_order() {
        let roots = roots(Path::new("/c"), Path::new("/p"));
        assert_eq!(
            roots.rosters(),
            vec![
                PathBuf::from("/c/providers.toml"),
                PathBuf::from("/p/providers.toml")
            ]
        );
    }
}
