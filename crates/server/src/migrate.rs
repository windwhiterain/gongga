//! Bringing a home's database up to the version this build expects.
//!
//! A home records the version it was last migrated to in `<home>/version`, and a
//! migrator moves it one step. The migrators are the one thing the install pass
//! does not place: they live in the shipped content under `migrate/<from>-to-<to>.py`
//! and are fetched by path when they are needed, because a home that is two
//! versions behind asks for the step it is missing, not for whatever the current
//! pin happens to carry. See `docs/notes/server-migrate.md`.
//!
//! # Invariant
//!
//! A migrator runs before the database is opened, exits zero, and only then does
//! the version file move. A step that failed is therefore run again on the next
//! start, which is why every migrator has to be safe to run twice.

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, bail};

/// The version this build reads and writes.
pub const VERSION: u32 = 2;

/// The file that says what version a home is at. Missing means 0: a home this
/// server has never migrated is the one it was created by.
const STAMP: &str = "version";

/// Where fetched migrators land, so the next start runs the same bytes.
const DIRECTORY: &str = "migrate";

/// What one start did, for the log line.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Report {
    /// The version the home was at when the process started.
    pub from: u32,
    /// The steps that ran, in order, as `0-to-1`.
    pub steps: Vec<String>,
}

/// The version a home is at: 0 when it has never been migrated.
pub fn version(home: &Path) -> u32 {
    std::fs::read_to_string(home.join(STAMP))
        .ok()
        .and_then(|text| text.trim().parse().ok())
        .unwrap_or(0)
}

/// Whether a migrator has to be found: not for a home at this version, nor for
/// a first start.
pub fn needs_content(home: &Path, db: &Path) -> bool {
    version(home) < VERSION && (db.is_file() || home.join(STAMP).is_file())
}

/// Runs every step between this home's version and [`VERSION`].
pub fn bring_forward(home: &Path, db: &Path) -> anyhow::Result<Report> {
    let mut report = Report {
        from: version(home),
        ..Default::default()
    };
    if report.from >= VERSION {
        return Ok(report);
    }
    // A home with no database has nothing to migrate: it is this build's first start.
    if !db.is_file() && !home.join(STAMP).is_file() {
        write_version(home, VERSION)?;
        return Ok(Report {
            from: VERSION,
            ..Default::default()
        });
    }
    for step in report.from..VERSION {
        let script = fetch(home, step)?;
        run(&script, home, db)?;
        // Only once it exited zero: a failed step is run again next start.
        write_version(home, step + 1)?;
        report.steps.push(format!("{step}-to-{}", step + 1));
    }
    Ok(report)
}

fn write_version(home: &Path, version: u32) -> anyhow::Result<()> {
    std::fs::write(home.join(STAMP), format!("{version}\n"))
        .with_context(|| format!("recording version {version} in {}", home.display()))
}

/// The migrator for one step, materialised inside the home.
fn fetch(home: &Path, from: u32) -> anyhow::Result<PathBuf> {
    let name = format!("{from}-to-{}.py", from + 1);
    let path = format!("{DIRECTORY}/{name}");
    let target = home.join(DIRECTORY).join(&name);
    let bytes = blob(home, &path)?
        .or_else(|| newest_blob(home, &path))
        .or_else(|| checkout_copy(home, &path))
        .ok_or_else(|| {
            anyhow::anyhow!(
                "this home is at version {from} and needs `{path}`, which is neither in the \
                 pinned content (pin {}) nor anywhere else this server can reach: upgrade the \
                 pin, or start the server where the shipped content lives",
                crate::ship::pinned().commit
            )
        })?;
    if let Some(parent) = target.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating {} for the migrator", parent.display()))?;
    }
    std::fs::write(&target, bytes)
        .with_context(|| format!("writing the migrator to {}", target.display()))?;
    Ok(target)
}

/// The path as the pinned commit has it.
fn blob(home: &Path, path: &str) -> anyhow::Result<Option<Vec<u8>>> {
    let cache = home.join("ship.git");
    if !cache.is_dir() {
        return Ok(None);
    }
    Ok(crate::ship::blob(
        &cache,
        &crate::ship::pinned().commit,
        path,
    ))
}

/// The path as the newest commit in the cache has it: the pin may predate a step
/// a newer install already knows, and a home stuck behind it needs the file, not
/// the argument about which pin is current.
fn newest_blob(home: &Path, path: &str) -> Option<Vec<u8>> {
    let cache = home.join("ship.git");
    if !cache.is_dir() {
        return None;
    }
    let listed = Command::new("git")
        .arg("--git-dir")
        .arg(&cache)
        .args(["rev-list", "--all", "--", path])
        .output()
        .ok()?;
    if !listed.status.success() {
        return None;
    }
    for commit in String::from_utf8_lossy(&listed.stdout).lines() {
        let commit = commit.trim();
        if commit.is_empty() {
            continue;
        }
        if let Some(bytes) = crate::ship::blob(&cache, commit, path) {
            return Some(bytes);
        }
    }
    None
}

/// A local checkout the pin names: `url` may be a path rather than a remote.
fn checkout_copy(home: &Path, path: &str) -> Option<Vec<u8>> {
    let url = PathBuf::from(&crate::ship::pinned().url);
    let _ = home;
    url.join(path)
        .is_file()
        .then(|| std::fs::read(url.join(path)).unwrap_or_default())
}

/// Runs one migrator: the home it may rewrite, and the database it changes.
fn run(script: &Path, home: &Path, db: &Path) -> anyhow::Result<()> {
    let python = interpreter()?;
    let output = Command::new(&python)
        .arg(script)
        .arg("--home")
        .arg(home)
        .arg("--db")
        .arg(db)
        .output()
        .with_context(|| format!("running {}", script.display()))?;
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    if !output.status.success() {
        bail!(
            "{} failed ({}): {}",
            script.display(),
            output.status,
            text.trim()
        );
    }
    if !text.trim().is_empty() {
        tracing::info!(migrator = %script.display(), said = %text.trim(), "migrator");
    }
    Ok(())
}

/// The interpreter this machine has: `python3`, then `python`, because Windows
/// installs the second name and Unix the first.
///
/// Found by asking, not by looking: Windows ships an execution alias named
/// `python3.exe` that exists on PATH and exits 9009 when the Store app behind it
/// was never installed, so a name on PATH is not a Python.
fn interpreter() -> anyhow::Result<PathBuf> {
    for name in ["python3", "python"] {
        if let Ok(found) = which::which(name) {
            let answers = Command::new(&found)
                .arg("-V")
                .output()
                .map(|out| out.status.success())
                .unwrap_or(false);
            if answers {
                return Ok(found);
            }
        }
    }
    bail!("a migrator needs python3 or python on PATH, and neither answers `-V`")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A home that never migrated reads as 0, and one that did says what it says.
    #[test]
    fn the_version_file_is_the_state() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(version(dir.path()), 0);
        write_version(dir.path(), 1).unwrap();
        assert_eq!(version(dir.path()), 1);
        // Nonsense is not a version, and 0 is the safe reading of it.
        std::fs::write(dir.path().join(STAMP), "not a number").unwrap();
        assert_eq!(version(dir.path()), 0);
    }

    /// A home already at this version runs nothing, and says so.
    #[test]
    fn a_current_home_is_left_alone() {
        let dir = tempfile::tempdir().unwrap();
        write_version(dir.path(), VERSION).unwrap();
        let report = bring_forward(dir.path(), &dir.path().join("g.db")).unwrap();
        assert_eq!(report.from, VERSION);
        assert!(report.steps.is_empty());
    }

    /// A cache holding `names`, as an install would have left one.
    fn cache_with(home: &Path, names: &[&str], body: &str) {
        let work = home.join("content");
        std::fs::create_dir_all(work.join(DIRECTORY)).unwrap();
        for name in names {
            std::fs::write(work.join(DIRECTORY).join(name), body).unwrap();
        }
        let git = |args: &[&str]| {
            let out = Command::new("git")
                .arg("-C")
                .arg(&work)
                .args(args)
                .output()
                .unwrap();
            assert!(
                out.status.success(),
                "git {args:?}: {}",
                String::from_utf8_lossy(&out.stderr)
            );
        };
        git(&["init", "-q", "-b", "main"]);
        git(&["add", "-A"]);
        git(&[
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@example.com",
            "commit",
            "-q",
            "-m",
            "content",
        ]);
        std::fs::rename(work.join(".git"), home.join("ship.git")).unwrap();
    }

    /// A first start is not a migration: there is nothing to bring forward.
    #[test]
    fn a_home_with_no_database_is_stamped_without_asking_for_a_migrator() {
        let dir = tempfile::tempdir().unwrap();
        let report = bring_forward(dir.path(), &dir.path().join("g.db")).unwrap();
        assert!(report.steps.is_empty(), "{report:?}");
        assert_eq!(report.from, VERSION);
        assert_eq!(version(dir.path()), VERSION);
    }

    /// The migrator file for every step this build asks for.
    fn every_step() -> Vec<String> {
        (0..VERSION)
            .map(|step| format!("{step}-to-{}.py", step + 1))
            .collect()
    }

    /// The version moves only once the step has exited zero.
    #[test]
    fn a_step_that_succeeds_moves_the_version() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path();
        std::fs::write(home.join("g.db"), "bytes").unwrap();
        let files = every_step();
        cache_with(
            home,
            &files.iter().map(String::as_str).collect::<Vec<_>>(),
            "import sys\nsys.exit(0)\n",
        );
        let report = bring_forward(home, &home.join("g.db")).unwrap();
        let steps: Vec<String> = files
            .iter()
            .map(|file| file.trim_end_matches(".py").to_string())
            .collect();
        assert_eq!(report.steps, steps, "{report:?}");
        assert_eq!(version(home), VERSION);
    }

    /// A step that fails leaves the home at the version it is really in.
    #[test]
    fn a_migrator_that_fails_leaves_the_version_where_it_was() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path();
        std::fs::write(home.join("g.db"), "bytes").unwrap();
        cache_with(home, &["0-to-1.py"], "import sys\nsys.exit(3)\n");
        let error = bring_forward(home, &home.join("g.db"))
            .unwrap_err()
            .to_string();
        assert!(error.contains("0-to-1.py"), "{error}");
        assert_eq!(version(home), 0, "a failed step is run again next start");
    }

    /// No Python is an error with the step's name in it, not a panic and not a
    /// silent fresh start on a home whose data is a version old.
    #[test]
    fn a_step_without_its_migrator_names_what_is_missing() {
        let dir = tempfile::tempdir().unwrap();
        let error = fetch(dir.path(), 0).unwrap_err().to_string();
        assert!(error.contains("migrate/0-to-1.py"), "{error}");
        assert!(error.contains("version 0"), "{error}");
    }
}
