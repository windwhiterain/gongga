//! What the harness ships, and how a home gets it from the content repository.
//!
//! See `docs/notes/server-ship.md`.

use std::path::{Component, Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;

use serde::Deserialize;

/// The pin compiled into this binary: which repository, which commit, what means
/// what there.
const MANIFEST: &str = include_str!("../ship.toml");

/// The file naming the content a root was last reconciled against.
const PIN_FILE: &str = ".ship-pin";

/// Where a home keeps the content it fetched, so the next start needs no network.
const CACHE: &str = "ship.git";

#[derive(Debug, Deserialize)]
struct Manifest {
    ship: Ship,
}

/// One pinned version of the shipped content.
#[derive(Debug, Clone, Deserialize)]
pub struct Ship {
    /// Where to fetch: any git URL, or a local checkout while developing.
    pub url: String,
    /// A full sha, so the name cannot slide to a later commit.
    pub commit: String,
    /// The repository path that becomes a home's `config.toml`.
    pub config: String,
    #[serde(default)]
    skills: SkillRoots,
}

/// Where each bucket of skills belongs.
#[derive(Debug, Clone, Default, Deserialize)]
struct SkillRoots {
    harness: String,
    general: String,
}

/// The pin this build ships. A manifest that does not parse is a build bug, not
/// a runtime condition.
pub fn pinned() -> &'static Ship {
    static PIN: OnceLock<Ship> = OnceLock::new();
    PIN.get_or_init(|| match toml::from_str::<Manifest>(MANIFEST) {
        Ok(m) => m.ship,
        Err(e) => panic!("crates/server/ship.toml does not parse: {e}"),
    })
}

/// Where content lands: a home owns the harness's config and its own
/// skills, the machine owns the general ones.
#[derive(Debug, Clone)]
pub struct Roots {
    pub home: PathBuf,
    pub machine: PathBuf,
}

impl Roots {
    pub fn of(home: &Path) -> Self {
        Self {
            home: home.to_path_buf(),
            machine: machine_root(),
        }
    }
}

/// The machine's own agent directory.
fn machine_root() -> PathBuf {
    dirs::home_dir()
        .map(|h| h.join(".agents"))
        .unwrap_or_else(|| PathBuf::from(".agents"))
}

/// One repository path and where it belongs on disk.
struct Mapping {
    source: String,
    /// Where it lands under the root: the file itself, or the directory its
    /// contents are spread into.
    target: PathBuf,
    /// Whether the source is a tree rather than a single file.
    tree: bool,
}

/// What one install pass did, for the log line and for whoever has to act on it.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Report {
    /// Files placed because nothing local claimed the path.
    pub written: usize,
    /// Files upstream moved and nobody locally had touched.
    pub updated: usize,
    /// Files whose local edits and upstream changes merged cleanly.
    pub merged: usize,
    /// Files already identical to the pinned content.
    pub current: usize,
    /// Files upstream dropped that nobody had edited.
    pub removed: usize,
    /// Files left exactly as they are, because the merge would have had to choose.
    pub conflicts: Vec<String>,
    /// Files the reader authored where upstream also has one: not ours to
    /// touch, and no root with one counts reconciled.
    pub kept: Vec<String>,
    /// The pin the roots are now reconciled against.
    pub pin: String,
    /// Whether the pinned content arrived from the repository this run.
    pub fetched: bool,
}

/// Makes this home answerable to a lookup before anything is installed.
///
/// # Invariant
///
/// The fetch is best effort, and that is the point: a migrator has to be
/// findable before the content that ships in the same commit is installed, so a
/// network that answers nothing must not be what strands a home's data. No
/// network leaves the lookup to the caller, which names what it could not find.
pub fn prepare(home: &Path) {
    let roots = Roots::of(home);
    let _ = ROOTS.set(roots);
}

/// Fetches the pinned content only if this home cannot answer for itself.
///
/// # Invariant
///
/// Called by a caller that has already found out whether anything needs the
/// cache: a home at this build's version migrates nothing, so nothing it starts
/// needs a commit it does not have. See `migrate::needs_content`.
pub fn prepare_for_migration(home: &Path) {
    let cache = Roots::of(home).home.join(CACHE);
    if !have(&cache, &pinned().commit)
        && let Err(e) = fetch(&cache, &pinned().url)
    {
        tracing::warn!(error = %e, "cannot reach the content repository yet");
    }
}

/// Fetches the pinned content and reconciles it into both roots.
pub fn install(home: &Path) -> anyhow::Result<Report> {
    install_at(&Roots::of(home), pinned())
}

/// The same, with the roots and the pin named: what a test drives, so it never
/// writes into a real `~/.agents`.
pub fn install_at(roots: &Roots, ship: &Ship) -> anyhow::Result<Report> {
    let cache = roots.home.join(CACHE);
    // A pin is immutable: a fetch of a repository that has it adds nothing, and
    // it is a round trip in front of every start.
    let cached = have(&cache, &ship.commit);
    let fetched = if cached {
        false
    } else if fetch(&cache, &ship.url).is_ok() {
        true
    } else if has_commit(&cache, &ship.commit) {
        // It arrived by another route; being offline is not being broken.
        false
    } else {
        anyhow::bail!(
            "the shipped content is not installed here and cannot be fetched; \
             pin {} from {} -- there is no prompt to run a turn on without it",
            ship.commit,
            ship.url
        );
    };
    let _ = ROOTS.set(roots.clone());
    let mut report = Report {
        pin: ship.commit.clone(),
        fetched,
        ..Default::default()
    };
    for (root, mappings) in plans(roots, ship) {
        reconcile(&cache, &root, &mappings, ship, &mut report)?;
    }
    Ok(report)
}

/// The pinned config's own text, read from this home's cache.
///
/// # Invariant
///
/// This is where the prompt defaults come from: the prose is not copied into
/// this binary, so a home whose config never mentions `[prompts]` still gets the
/// pinned text, and one whose cache cannot answer gets nothing and says so.
pub fn config_text() -> Option<String> {
    let ship = pinned();
    blob(&roots().home.join(CACHE), &ship.commit, &ship.config)
        .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
}

/// Each root, with the repository paths that belong under it.
fn plans(roots: &Roots, ship: &Ship) -> Vec<(PathBuf, Vec<Mapping>)> {
    let home = vec![
        Mapping {
            source: ship.config.clone(),
            target: PathBuf::from("config.toml"),
            tree: false,
        },
        Mapping {
            source: ship.skills.harness.clone(),
            target: PathBuf::from("skills"),
            tree: true,
        },
    ];
    let machine = vec![Mapping {
        source: ship.skills.general.clone(),
        target: PathBuf::from("skills"),
        tree: true,
    }];
    vec![(roots.home.clone(), home), (roots.machine.clone(), machine)]
}

/// Reconciles one root against the pinned content.
///
/// # Invariant
///
/// A file is never overwritten while it differs from the content that installed
/// it: the last reconciled pin is the merge base, the disk is our side, the new
/// pin is theirs, and a conflict leaves the reader's file untouched and says so.
/// A root with a conflict keeps its old pin too, so the next start repeats the
/// same merge instead of treating an unmerged file as settled.
fn reconcile(
    cache: &Path,
    root: &Path,
    mappings: &[Mapping],
    ship: &Ship,
    report: &mut Report,
) -> anyhow::Result<()> {
    // The report is shared by both roots, so this root judges only what it adds.
    let conflicts_at = report.conflicts.len();
    let kept_at = report.kept.len();
    let old = read_pin(root);
    // Already at this pin, so upstream cannot have moved: one file read replaces
    // a `git` per installed file.
    if old.as_deref() == Some(ship.commit.as_str()) {
        return Ok(());
    }
    let new = collect(cache, &ship.commit, mappings, root);
    let before = old
        .as_deref()
        .map(|pin| collect(cache, pin, mappings, root))
        .unwrap_or_default();

    for file in &new {
        let Some(theirs) = blob(cache, &ship.commit, &file.source) else {
            continue;
        };
        let ours = std::fs::read(&file.dest).ok();
        let base = old
            .as_deref()
            .and_then(|pin| blob(cache, pin, &file.source));
        match decide(ours.as_deref(), base.as_deref(), &theirs) {
            Decision::Place => {
                put(&file.dest, &theirs, file.exec)?;
                report.written += 1;
            }
            Decision::Update => {
                put(&file.dest, &theirs, file.exec)?;
                report.updated += 1;
            }
            Decision::Adopt => {
                put(&file.dest, &theirs, file.exec)?;
                report.current += 1;
            }
            Decision::Merge(text) => {
                put(&file.dest, &text, file.exec)?;
                report.merged += 1;
            }
            Decision::Conflict => {
                report.conflicts.push(file.source.clone());
            }
            Decision::Keep => report.kept.push(file.source.clone()),
        }
    }

    for file in &before {
        if new.iter().any(|kept| kept.source == file.source) {
            continue;
        }
        let Some(base) = old
            .as_deref()
            .and_then(|pin| blob(cache, pin, &file.source))
        else {
            continue;
        };
        match std::fs::read(&file.dest).ok() {
            Some(ours) if ours == base => {
                let _ = std::fs::remove_file(&file.dest);
                report.removed += 1;
            }
            Some(_) => {
                report
                    .kept
                    .push(format!("{} (kept: removed upstream)", file.source));
            }
            None => {}
        }
    }

    // A conflict or a kept file means the root is not reconciled: its base has
    // to stay what actually installed here.
    if report.conflicts.len() == conflicts_at && report.kept.len() == kept_at {
        write_pin(root, &ship.commit)?;
    }
    Ok(())
}

/// One installed file: where it came from and where it sits.
struct Planned {
    source: String,
    dest: PathBuf,
    exec: bool,
}

/// Every pinned file a set of mappings accounts for, as a path under `root`.
fn collect(cache: &Path, commit: &str, mappings: &[Mapping], root: &Path) -> Vec<Planned> {
    let mut out = Vec::new();
    for mapping in mappings {
        for (path, exec) in listed(cache, commit, &mapping.source) {
            let dest = if mapping.tree {
                let Some(rel) = under(&mapping.source, &path).and_then(|r| safe_relpath(&r)) else {
                    tracing::warn!(path = %path, "a shipped path is not installable; skipped");
                    continue;
                };
                root.join(&mapping.target).join(rel)
            } else {
                root.join(&mapping.target)
            };
            out.push(Planned {
                source: path,
                dest,
                exec,
            });
        }
    }
    out
}

/// `ls-tree -r` for one repository path, which may be a file or a directory.
fn listed(cache: &Path, commit: &str, path: &str) -> Vec<(String, bool)> {
    let Ok(out) = Command::new(git())
        .arg("--git-dir")
        .arg(cache)
        .args(["ls-tree", "-r", "--long", commit])
        .arg("--")
        .arg(path)
        .output()
    else {
        return Vec::new();
    };
    if !out.status.success() {
        return Vec::new();
    }
    // "<mode> <type> <object> <size>	<path>", one line each.
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|line| {
            let (head, path) = line.split_once('\t')?;
            let exec = head.split(' ').next()? == "100755";
            Some((path.to_string(), exec))
        })
        .collect()
}

/// What to do with one file, given its three sides.
enum Decision {
    /// Nothing local claims it: place the pinned content.
    Place,
    /// Upstream moved it and the local copy still matched what installed it.
    Update,
    /// Identical already, so only the executable bit may need settling.
    Adopt,
    /// Both sides moved and git joined them.
    Merge(Vec<u8>),
    /// The reader's file stands, and the root keeps its old pin.
    Conflict,
    /// A file the reader made at a path upstream also has: not ours to touch.
    Keep,
}

fn decide(ours: Option<&[u8]>, base: Option<&[u8]>, theirs: &[u8]) -> Decision {
    match (ours, base) {
        (None, _) => Decision::Place,
        (Some(ours), _) if ours == theirs => Decision::Adopt,
        // No merge base: the reader wrote this file, and upstream has one at the
        // same path. Neither may overwrite the other.
        (Some(_), None) => Decision::Keep,
        (Some(ours), Some(base)) if ours == base => Decision::Update,
        (Some(ours), Some(base)) => match merge_text(ours, base, theirs) {
            Some(merged) => Decision::Merge(merged),
            None => Decision::Conflict,
        },
    }
}

/// `git merge-file` across three temp files; `None` is a conflict or a failure,
/// and both keep the reader's file.
fn merge_text(ours: &[u8], base: &[u8], theirs: &[u8]) -> Option<Vec<u8>> {
    let dir = std::env::temp_dir().join(format!(
        "gongga-ship-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    if std::fs::create_dir_all(&dir).is_err() {
        return None;
    }
    let (o, b, t) = (dir.join("ours"), dir.join("base"), dir.join("theirs"));
    let _ = std::fs::write(&o, ours);
    let _ = std::fs::write(&b, base);
    let _ = std::fs::write(&t, theirs);
    let merged = Command::new(git())
        .args(["merge-file", "-p"])
        .arg(&o)
        .arg(&b)
        .arg(&t)
        .output()
        .ok()
        .filter(|out| out.status.code().is_some_and(|c| c == 0))
        .map(|out| out.stdout);
    let _ = std::fs::remove_dir_all(&dir);
    merged
}

/// The path one file sits at inside a mapped directory, if it is under it.
fn under(prefix: &str, path: &str) -> Option<String> {
    let prefix = prefix.trim_end_matches('/');
    path.strip_prefix(prefix)
        .map(|rest| rest.trim_matches('/').to_string())
        .filter(|rest| !rest.is_empty())
}

/// Where one repository path belongs under a root, refusing anything that climbs.
///
/// # Safety
///
/// The path is read out of a repository someone else pushed to, so `..`, an
/// absolute root and a drive prefix are refused rather than joined.
fn safe_relpath(path: &str) -> Option<PathBuf> {
    let mut out = PathBuf::new();
    for c in Path::new(path).components() {
        match c {
            Component::Normal(part) => out.push(part),
            Component::CurDir => {}
            _ => return None,
        }
    }
    (!out.as_os_str().is_empty()).then_some(out)
}

pub fn blob(cache: &Path, commit: &str, path: &str) -> Option<Vec<u8>> {
    Command::new(git())
        .arg("--git-dir")
        .arg(cache)
        .args(["cat-file", "blob"])
        .arg(format!("{commit}:{path}"))
        .output()
        .ok()
        .filter(|out| out.status.success())
        .map(|out| out.stdout)
}

fn read_pin(root: &Path) -> Option<String> {
    std::fs::read_to_string(root.join(PIN_FILE))
        .ok()
        .map(|t| t.trim().to_string())
        .filter(|t| !t.is_empty())
}

fn write_pin(root: &Path, commit: &str) -> anyhow::Result<()> {
    std::fs::create_dir_all(root)?;
    std::fs::write(root.join(PIN_FILE), format!("{commit}\n"))?;
    Ok(())
}

/// Writes a shipped file, keeping the mode the repository recorded for it.
fn put(dest: &Path, bytes: &[u8], exec: bool) -> anyhow::Result<()> {
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(dest, bytes)?;
    #[cfg(unix)]
    if exec {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(dest)
            .map(|m| m.permissions().mode())
            .unwrap_or(0o644);
        let _ = std::fs::set_permissions(dest, std::fs::Permissions::from_mode(mode | 0o111));
    }
    #[cfg(not(unix))]
    let _ = exec;
    Ok(())
}

/// Fetches every branch into a bare cache.
///
/// # Invariant
///
/// Branches rather than the pinned sha: a server should not fail because a
/// repository declines to serve an object by name, and a pin unreachable from any
/// branch is a mistake in `ship.toml` rather than a condition to handle.
fn fetch(cache: &Path, url: &str) -> anyhow::Result<()> {
    if !cache.exists() {
        if let Some(parent) = cache.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let init = Command::new(git())
            .arg("--git-dir")
            .arg(cache)
            .args(["init", "-q", "--bare"])
            .status()?;
        if !init.success() {
            anyhow::bail!("cannot create the content cache at {}", cache.display());
        }
    }
    let out = Command::new(git())
        .arg("--git-dir")
        .arg(cache)
        .args(["fetch", "-q", url])
        .arg("+refs/heads/*:refs/heads/ship/*")
        .output()?;
    if !out.status.success() {
        anyhow::bail!(
            "git fetch failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(())
}

/// Whether the cache already holds this pin, so no network is needed for it.
pub fn have(cache: &Path, commit: &str) -> bool {
    cache.is_dir() && has_commit(cache, commit)
}

fn has_commit(cache: &Path, commit: &str) -> bool {
    Command::new(git())
        .arg("--git-dir")
        .arg(cache)
        .args(["cat-file", "-e"])
        .arg(format!("{commit}^{{commit}}"))
        .output()
        .map(|out| out.status.success())
        .unwrap_or(false)
}

/// The roots this process installed into.
static ROOTS: OnceLock<Roots> = OnceLock::new();

/// Where the skills are: the roots of the last install, or the home this process
/// would have used if it had installed.
pub fn roots() -> &'static Roots {
    ROOTS.get_or_init(|| {
        let home = std::env::var_os("GONGGA_HOME")
            .map(PathBuf::from)
            .or_else(|| dirs::home_dir().map(|h| h.join(".gongga")))
            .unwrap_or_else(|| PathBuf::from(".gongga"));
        Roots::of(&home)
    })
}

/// `git` by name: it is what the machine has, and a path resolved at build time
/// is wrong on the next one.
pub(crate) fn git() -> &'static str {
    "git"
}

#[cfg(test)]
mod tests {
    use super::*;

    fn git_in(dir: &Path, args: &[&str]) {
        let out = Command::new(git())
            .arg("-C")
            .arg(dir)
            .args(["-c", "user.email=test@gongga", "-c", "user.name=test"])
            .args(args)
            .output()
            .expect("the tests need git on PATH");
        assert!(out.status.success(), "{args:?}: {out:?}");
    }

    fn head(repo: &Path) -> String {
        let out = Command::new(git())
            .arg("-C")
            .arg(repo)
            .args(["rev-parse", "HEAD"])
            .output()
            .unwrap()
            .stdout;
        String::from_utf8(out).unwrap().trim().to_string()
    }

    /// A repository holding shipped content, shaped like the real one.
    fn content_repo(dir: &Path, config: &str, skill: &str) -> String {
        std::fs::create_dir_all(dir.join("config")).unwrap();
        std::fs::create_dir_all(dir.join("skills/harness/heir")).unwrap();
        std::fs::create_dir_all(dir.join("skills/general/remote")).unwrap();
        std::fs::write(dir.join("config/config.toml"), config).unwrap();
        std::fs::write(dir.join("skills/harness/heir/SKILL.md"), skill).unwrap();
        std::fs::write(dir.join("skills/general/remote/remote.py"), skill).unwrap();
        git_in(dir, &["init", "-q", "-b", "main"]);
        git_in(dir, &["add", "-A"]);
        git_in(dir, &["commit", "-q", "-m", "content"]);
        head(dir)
    }

    fn ship_at(url: &str, commit: &str) -> Ship {
        Ship {
            url: url.to_string(),
            commit: commit.to_string(),
            config: "config/config.toml".to_string(),
            skills: SkillRoots {
                harness: "skills/harness".to_string(),
                general: "skills/general".to_string(),
            },
        }
    }

    /// A throwaway home, machine root and content repository: no test here writes
    /// into the real `~/.agents`.
    struct Fixture {
        roots: Roots,
        content: PathBuf,
    }

    fn fixture(dir: &Path) -> Fixture {
        let content = dir.join("content");
        let home = dir.join("home");
        let machine = dir.join("machine");
        std::fs::create_dir_all(&home).unwrap();
        std::fs::create_dir_all(&machine).unwrap();
        Fixture {
            roots: Roots { home, machine },
            content,
        }
    }

    impl Fixture {
        fn install(&self, commit: &str) -> anyhow::Result<Report> {
            install_at(
                &self.roots,
                &ship_at(self.content.to_str().unwrap(), commit),
            )
        }

        fn at(&self) -> Report {
            self.install(&head(&self.content)).unwrap()
        }

        fn home(&self) -> &Path {
            &self.roots.home
        }

        fn machine(&self) -> &Path {
            &self.roots.machine
        }

        fn upstream(&self, text: &str) {
            std::fs::write(self.content.join("config/config.toml"), text).unwrap();
            git_in(&self.content, &["commit", "-q", "-am", "upstream edit"]);
        }

        fn pin(&self, root: &Path) -> String {
            std::fs::read_to_string(root.join(PIN_FILE))
                .unwrap()
                .trim()
                .to_string()
        }
    }

    /// The manifest is data this binary trusts, so a short sha would stop naming
    /// one commit the moment the repository moves.
    #[test]
    fn the_pin_names_a_full_commit_and_two_buckets() {
        let ship = pinned();
        assert_eq!(ship.commit.len(), 40, "{}", ship.commit);
        assert!(
            ship.commit.chars().all(|c| c.is_ascii_hexdigit()),
            "{}",
            ship.commit
        );
        assert!(
            ship.url.starts_with("https://") || Path::new(&ship.url).exists(),
            "a git url, or a local checkout while developing"
        );
        assert_eq!(ship.config, "config/config.toml");
        assert_eq!(ship.skills.harness, "skills/harness");
        assert_eq!(ship.skills.general, "skills/general");
    }

    #[test]
    fn a_first_start_lands_the_config_and_both_skill_buckets() {
        let d = tempfile::tempdir().unwrap();
        let f = fixture(d.path());
        let commit = content_repo(&f.content, "instructions here\n", "# skill\n");
        let report = f.install(&commit).unwrap();

        assert_eq!(
            std::fs::read_to_string(f.home().join("config.toml")).unwrap(),
            "instructions here\n"
        );
        assert!(f.home().join("skills/heir/SKILL.md").exists());
        assert!(f.machine().join("skills/remote/remote.py").exists());
        assert_eq!(report.written, 3, "{report:?}");
        assert!(report.conflicts.is_empty(), "{report:?}");
        assert!(report.fetched);
        assert_eq!(f.pin(f.home()), commit);
        assert_eq!(f.pin(f.machine()), commit);
    }

    /// A root already at the pin is left alone, and not even read.
    #[test]
    fn a_second_start_with_the_same_pin_writes_nothing() {
        let d = tempfile::tempdir().unwrap();
        let f = fixture(d.path());
        let commit = content_repo(&f.content, "a\n", "b\n");
        let first = f.install(&commit).unwrap();
        assert!(first.written > 0, "{first:?}");
        let again = f.at();
        assert_eq!(again.written, 0, "{again:?}");
        assert_eq!(again.updated, 0, "{again:?}");
        assert_eq!(again.merged, 0, "{again:?}");
        assert_eq!(again.removed, 0, "{again:?}");
        assert!(
            again.conflicts.is_empty() && again.kept.is_empty(),
            "{again:?}"
        );
        assert_eq!(f.pin(f.home()), commit);
        // Counted as nothing at all: it did not look.
        assert_eq!(again.current, 0, "{again:?}");
    }

    /// Why a merge exists: both sides keep what they wrote, on lines git cannot
    /// read as an overlap.
    #[test]
    fn a_local_edit_and_an_upstream_edit_merge_into_one_file() {
        let d = tempfile::tempdir().unwrap();
        let f = fixture(d.path());
        let first = content_repo(&f.content, "one\ntwo\nthree\nfour\nfive\nsix\n", "s\n");
        f.install(&first).unwrap();

        std::fs::write(
            f.home().join("config.toml"),
            "one\nmine\nthree\nfour\nfive\nsix\n",
        )
        .unwrap();
        f.upstream("one\ntwo\nthree\nfour\ntheirs\nsix\n");
        let report = f.at();

        assert_eq!(
            std::fs::read_to_string(f.home().join("config.toml")).unwrap(),
            "one\nmine\nthree\nfour\ntheirs\nsix\n",
            "{report:?}"
        );
        assert_eq!(report.merged, 1, "{report:?}");
        assert!(report.conflicts.is_empty(), "{report:?}");
    }

    /// A home with its own config keeps it, is told on every start, and holds
    /// the pin it actually reconciled.
    #[test]
    fn a_config_the_reader_wrote_is_kept_and_said() {
        let d = tempfile::tempdir().unwrap();
        let f = fixture(d.path());
        let commit = content_repo(&f.content, "shipped\n", "s\n");
        std::fs::write(f.home().join("config.toml"), "mine\n").unwrap();

        let report = f.install(&commit).unwrap();
        assert_eq!(
            std::fs::read_to_string(f.home().join("config.toml")).unwrap(),
            "mine\n"
        );
        assert_eq!(
            report.kept,
            vec!["config/config.toml".to_string()],
            "{report:?}"
        );
        assert!(!f.home().join(PIN_FILE).exists(), "nothing was reconciled");
        // The skills it never had still arrive.
        assert!(f.home().join("skills/heir/SKILL.md").exists());
    }

    #[test]
    fn an_upstream_edit_nobody_locally_touched_just_lands() {
        let d = tempfile::tempdir().unwrap();
        let f = fixture(d.path());
        let first = content_repo(&f.content, "one\n", "s\n");
        f.install(&first).unwrap();
        f.upstream("one\ntwo\n");
        let report = f.at();
        assert_eq!(report.updated, 1, "{report:?}");
        assert_eq!(
            std::fs::read_to_string(f.home().join("config.toml")).unwrap(),
            "one\ntwo\n"
        );
    }

    /// Both sides the same line: the reader keeps the file and the pin stays,
    /// so the next start asks again.
    #[test]
    fn a_conflict_keeps_the_readers_file_and_the_old_pin() {
        let d = tempfile::tempdir().unwrap();
        let f = fixture(d.path());
        let first = content_repo(&f.content, "shared\n", "s\n");
        f.install(&first).unwrap();

        std::fs::write(f.home().join("config.toml"), "mine\n").unwrap();
        f.upstream("theirs\n");
        let second = head(&f.content);
        let report = f.install(&second).unwrap();

        assert_eq!(
            std::fs::read_to_string(f.home().join("config.toml")).unwrap(),
            "mine\n"
        );
        assert_eq!(report.conflicts, vec!["config/config.toml".to_string()]);
        assert_eq!(f.pin(f.home()), first, "the merge base has to stay");

        let again = f.install(&second).unwrap();
        assert_eq!(
            again.conflicts, report.conflicts,
            "it asks again, not decides"
        );
    }

    #[test]
    fn a_file_upstream_dropped_and_nobody_touched_is_removed() {
        let d = tempfile::tempdir().unwrap();
        let f = fixture(d.path());
        let first = content_repo(&f.content, "c\n", "skill text\n");
        f.install(&first).unwrap();

        std::fs::remove_file(f.content.join("skills/harness/heir/SKILL.md")).unwrap();
        git_in(&f.content, &["add", "-A"]);
        git_in(&f.content, &["commit", "-q", "-m", "drop a skill"]);
        let report = f.at();

        assert!(!f.home().join("skills/heir/SKILL.md").exists());
        assert_eq!(report.removed, 1, "{report:?}");
    }

    #[test]
    fn a_file_the_reader_edited_survives_being_dropped_upstream() {
        let d = tempfile::tempdir().unwrap();
        let f = fixture(d.path());
        let first = content_repo(&f.content, "c\n", "skill text\n");
        f.install(&first).unwrap();
        std::fs::write(f.home().join("skills/heir/SKILL.md"), "mine now\n").unwrap();

        std::fs::remove_file(f.content.join("skills/harness/heir/SKILL.md")).unwrap();
        git_in(&f.content, &["add", "-A"]);
        git_in(&f.content, &["commit", "-q", "-m", "drop a skill"]);
        let report = f.at();

        assert_eq!(
            std::fs::read_to_string(f.home().join("skills/heir/SKILL.md")).unwrap(),
            "mine now\n"
        );
        assert_eq!(
            report.kept,
            vec!["skills/harness/heir/SKILL.md (kept: removed upstream)".to_string()],
            "{report:?}"
        );
        assert!(report.conflicts.is_empty(), "{report:?}");
        assert_eq!(report.removed, 0);
    }

    /// The refusal that stops a promptless agent from looking healthy.
    #[test]
    fn an_unreachable_pin_and_nothing_installed_refuses_to_start() {
        let d = tempfile::tempdir().unwrap();
        let f = fixture(d.path());
        let e = install_at(
            &f.roots,
            &ship_at(d.path().join("nowhere").to_str().unwrap(), &"0".repeat(40)),
        )
        .expect_err("a fresh machine with no source has no prompts to serve");
        assert!(e.to_string().contains("cannot be fetched"), "{e}");
    }

    /// An update that cannot check in still runs what is here: the pin's objects
    /// were fetched once.
    #[test]
    fn a_lost_repository_still_serves_what_it_installed() {
        let d = tempfile::tempdir().unwrap();
        let f = fixture(d.path());
        let commit = content_repo(&f.content, "kept\n", "s\n");
        f.install(&commit).unwrap();
        std::fs::remove_dir_all(&f.content).unwrap();

        let report = f.install(&commit).unwrap();
        assert!(!report.fetched, "the fetch failed this time");
        assert_eq!(
            std::fs::read_to_string(f.home().join("config.toml")).unwrap(),
            "kept\n"
        );
    }

    /// A repository path is somebody else's string, so it may not choose where it
    /// lands.
    #[test]
    fn a_shipped_path_cannot_climb_out_of_a_root() {
        assert_eq!(
            safe_relpath("skills/a/SKILL.md").unwrap(),
            PathBuf::from("skills/a/SKILL.md")
        );
        for escape in [
            "../evil",
            "skills/../../evil",
            "/etc/passwd",
            "C:/Windows",
            ".",
        ] {
            assert!(safe_relpath(escape).is_none(), "{escape} installed anyway");
        }
    }
}
