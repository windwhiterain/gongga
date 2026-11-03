//! What the harness ships, and how a home gets it from the content repository.
//!
//! See `docs/notes/server-ship.md`.

use std::path::{Component, Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;

use serde::Deserialize;

/// What this build expects of the content: where it comes from, which branch is
/// current, and which protocol that content speaks.
///
/// # Invariant
///
/// No commit is named here. The server asks the branch for its newest commit that
/// speaks this protocol, so a content change -- a skill's prose, a new skill, a
/// path retired -- reaches a machine by a restart. `protocol` moves only when the
/// *shape* of the content moves in a way this build cannot read, and that is the
/// one content change which needs a new build.
const MANIFEST: &str = include_str!("../ship.toml");

/// The file the content declares itself in, at the commit it is read from.
const DECLARATION: &str = "ship.toml";

/// The file naming the content a root was last reconciled against.
const PIN_FILE: &str = ".ship-pin";

/// Where a home keeps the content it fetched, so the next start needs no network.
const CACHE: &str = "ship.git";

#[derive(Debug, Deserialize)]
struct Manifest {
    ship: Link,
}

/// Where the content comes from, and which protocol this build reads.
#[derive(Debug, Clone, Deserialize)]
pub struct Link {
    /// Where to fetch: any git URL, or a local checkout while developing.
    pub url: String,
    /// The branch whose newest matching commit is current.
    pub branch: String,
    /// The content protocol this build reads.
    pub protocol: u32,
}

/// What the content says about itself: which protocol it is, and where its parts
/// belong in a home.
///
/// # Invariant
///
/// The mapping is content, not code. A retired path or a renamed skills root is a
/// fact about this repository's own history, and keeping it in the binary would
/// mean a rebuild for exactly the kind of edit this design exists to make free.
#[derive(Debug, Clone, Deserialize)]
struct Declared {
    /// The protocol this content speaks, which is how a commit is chosen.
    protocol: u32,
    /// The repository path that becomes a home's `config.toml`.
    config: String,
    #[serde(default)]
    skills: SkillRoots,
}

/// One commit of the content, with what it declares.
#[derive(Debug, Clone)]
pub struct Content {
    /// The full sha a home reconciles against, and what `.ship-pin` records.
    pub commit: String,
    /// Whether the repository answered for this resolve.
    pub fetched: bool,
    declared: Declared,
}

/// Where each bucket of skills belongs.
#[derive(Debug, Clone, Default, Deserialize)]
struct SkillRoots {
    /// Where this repository keeps the skills it owns, one dir per skill.
    harness: String,
    /// Paths an older commit installed and this one does not, so their copies go.
    ///
    /// # Invariant
    ///
    /// Each one still names the root it was installed under, because that is
    /// where its files have to be found to be recognised as ours.
    #[serde(default)]
    retired: Vec<Retired>,
}

#[derive(Debug, Clone, Deserialize)]
struct Retired {
    /// The repository path, which no longer exists.
    path: String,
    /// Which root it was installed under.
    root: RetiredRoot,
}

#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
enum RetiredRoot {
    /// The machine's `~/.agents`, which is now the community clone's alone.
    Machine,
}

/// The manifest this build carries. A manifest that does not parse is a build
/// bug, not a runtime condition.
pub fn link() -> &'static Link {
    static LINK: OnceLock<Link> = OnceLock::new();
    LINK.get_or_init(|| {
        let mut link = match toml::from_str::<Manifest>(MANIFEST) {
            Ok(m) => m.ship,
            Err(e) => panic!("crates/server/ship.toml does not parse: {e}"),
        };
        // A scratch repository is how a release is verified without publishing
        // one, and how a person develops against a local checkout. Same shape as
        // the roster's `GONGGA_PROVIDERS_CATALOG`: it names the one remote.
        if let Some(url) = std::env::var_os("GONGGA_SHIP_URL") {
            link.url = url.to_string_lossy().into_owned();
        }
        link
    })
}

/// Where content lands: a home owns the config and its skills. The machine's
/// skills are the community clone's.
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
    /// Files their own copy of another commit had installed, so upstream replaced them.
    pub rebased: Vec<String>,
    /// Files whose local edits and upstream changes merged cleanly.
    pub merged: usize,
    /// Files already identical to the installed content.
    pub current: usize,
    /// Files upstream dropped that nobody had edited.
    pub removed: usize,
    /// Files left exactly as they are, because the merge would have had to choose.
    pub conflicts: Vec<String>,
    /// Files the reader authored where upstream also has one: not ours to
    /// touch, and no root with one counts reconciled.
    pub kept: Vec<String>,
    /// The content commit the roots are now reconciled against.
    pub pin: String,
    /// Whether the repository answered this start.
    pub fetched: bool,
}

/// The content this start installs, resolved once for the whole process.
///
/// # Invariant
///
/// One answer for one start: the migrator, the install and the prompt defaults
/// all read this commit, so a fetch landing mid-start cannot make them disagree
/// about what "the content" is.
pub fn content(home: &Path) -> anyhow::Result<&'static Content> {
    match CONTENT.get_or_init(|| resolve(home).map_err(|e| format!("{e:#}"))) {
        Ok(content) => Ok(content),
        // The failure is cached too: every caller in this start asks the same
        // repository the same question, and one answer is enough.
        Err(e) => anyhow::bail!("{e}"),
    }
}

/// The commit this start resolved, when it resolved one.
pub fn resolved_commit() -> Option<&'static str> {
    CONTENT
        .get()
        .and_then(|resolved| resolved.as_ref().ok())
        .map(|content| content.commit.as_str())
}

/// One resolve per process, kept even when it failed.
static CONTENT: OnceLock<Result<Content, String>> = OnceLock::new();

/// Makes this home answerable to a lookup before anything is installed.
///
/// # Invariant
///
/// The resolve is best effort here, and that is the point: a migrator has to be
/// findable before the content that ships beside it is installed, so a network
/// that answers nothing must not be what strands a home's data. Whatever it could
/// not find is `install`'s to report, in the terms of what a home needs.
pub fn prepare(home: &Path) {
    let _ = ROOTS.set(Roots::of(home));
    let _ = content(home);
}

/// Fetches the content and reconciles it into both roots.
pub fn install(home: &Path) -> anyhow::Result<Report> {
    let content = content(home)?;
    install_at(&Roots::of(home), content)
}

/// The same, with the roots and the content named: what a test drives, so it never
/// writes into a real `~/.agents`.
pub fn install_at(roots: &Roots, content: &Content) -> anyhow::Result<Report> {
    let cache = roots.home.join(CACHE);
    // Resolved content is in this cache by construction. A caller that names a
    // commit nobody fetched is told so: believing a fetch's exit status instead
    // is how a home once came up with no `config.toml` at all, to fail much later
    // as "no prompt templates".
    if !has_commit(&cache, &content.commit) {
        anyhow::bail!(
            "the content commit {} is not in {}, so nothing can be installed from it",
            content.commit,
            cache.display()
        );
    }
    let mut report = Report {
        pin: content.commit.clone(),
        fetched: content.fetched,
        ..Default::default()
    };
    for (root, mappings, retired) in plans(roots, content) {
        reconcile(&cache, &root, &mappings, &retired, content, &mut report)?;
    }
    Ok(report)
}

/// The content's own config, as the resolved commit has it.
///
/// # Invariant
///
/// This is where the prompt defaults come from: the prose is not copied into this
/// binary, so a home whose config never mentions `[prompts]` still gets the
/// shipped text, and one whose content was never resolved gets nothing and says
/// so.
pub fn config_text() -> Option<String> {
    let content = content(&roots().home).ok()?;
    blob(
        &roots().home.join(CACHE),
        &content.commit,
        &content.declared.config,
    )
    .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
}

/// Resolves the branch's newest commit that speaks this build's protocol.
///
/// # Invariant
///
/// Protocol drift walks *back*, never forward: a content repository that has
/// moved to a protocol this build cannot read still serves this build the last
/// commit it could read. The alternative is a machine that cannot start because
/// somebody else's repository advanced.
fn resolve(home: &Path) -> anyhow::Result<Content> {
    let link = link();
    let cache = home.join(CACHE);
    // Best effort: a repository that does not answer leaves the cache answering,
    // because being offline is not being broken. A cache with nothing usable in
    // it is the one case that stops a start.
    let fetched = match fetch(&cache, &link.url) {
        Ok(()) => true,
        Err(e) => {
            tracing::warn!(error = %e, "cannot reach the content repository");
            false
        }
    };
    let tip = branch_tip(&cache, link)?;
    let commit = newest_for(&cache, &tip, link)?;
    let declared = declared_at(&cache, &commit)?;
    Ok(Content {
        commit,
        fetched,
        declared,
    })
}

/// The commit a branch points at in the cache.
fn branch_tip(cache: &Path, link: &Link) -> anyhow::Result<String> {
    let out = Command::new(git())
        .arg("--git-dir")
        .arg(cache)
        .args(["rev-parse", "--verify", "--quiet"])
        .arg(format!("refs/heads/ship/{}^{{commit}}", link.branch))
        .output()?;
    if !out.status.success() {
        anyhow::bail!(
            "{} has no branch {} to take content from, so protocol {} cannot be installed \
             (the cache is {})",
            link.url,
            link.branch,
            link.protocol,
            cache.display()
        );
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// The newest commit on the branch, following first parents, that speaks the
/// protocol this build reads.
fn newest_for(cache: &Path, tip: &str, link: &Link) -> anyhow::Result<String> {
    // The common case is the tip itself: one read instead of a walk.
    if protocol_at(cache, tip)? == Some(link.protocol) {
        return Ok(tip.to_string());
    }
    let out = Command::new(git())
        .arg("--git-dir")
        .arg(cache)
        .args(["rev-list", "--first-parent", tip])
        .output()?;
    if !out.status.success() {
        anyhow::bail!(
            "cannot read the history of {}: {}",
            link.url,
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    for commit in String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
    {
        if protocol_at(cache, commit)? == Some(link.protocol) {
            return Ok(commit.to_string());
        }
    }
    anyhow::bail!(
        "no commit on {} speaks protocol {}; the newest one speaks {} and this build reads \
         {}. Build a server that reads the new protocol, or publish content for this one",
        link.branch,
        link.protocol,
        protocol_at(cache, tip)?
            .map(|p| p.to_string())
            .unwrap_or_else(|| "nothing this build can parse".to_string()),
        link.protocol
    )
}

/// The protocol one commit declares, when it declares one this build can parse.
fn protocol_at(cache: &Path, commit: &str) -> anyhow::Result<Option<u32>> {
    let Some(bytes) = blob(cache, commit, DECLARATION) else {
        return Ok(None);
    };
    match toml::from_str::<Declared>(&String::from_utf8_lossy(&bytes)) {
        Ok(declared) => Ok(Some(declared.protocol)),
        Err(e) => {
            tracing::warn!(commit = %commit, error = %e, "the content declaration does not parse; skipped");
            Ok(None)
        }
    }
}

/// What one commit declares about itself.
fn declared_at(cache: &Path, commit: &str) -> anyhow::Result<Declared> {
    let bytes = blob(cache, commit, DECLARATION).ok_or_else(|| {
        anyhow::anyhow!(
            "commit {commit} of {} does not carry {DECLARATION}, so it cannot say where its \
             content belongs",
            link().url
        )
    })?;
    toml::from_str(&String::from_utf8_lossy(&bytes))
        .map_err(|e| anyhow::anyhow!("{DECLARATION} at {commit} does not parse: {e}"))
}

/// Each root, with the repository paths that belong under it now and the ones an
/// older commit put there.
fn plans(roots: &Roots, content: &Content) -> Vec<(PathBuf, Vec<Mapping>, Vec<Mapping>)> {
    let declared = &content.declared;
    let home = vec![
        Mapping {
            source: declared.config.clone(),
            target: PathBuf::from("config.toml"),
            tree: false,
        },
        Mapping {
            source: declared.skills.harness.clone(),
            target: PathBuf::from("skills"),
            tree: true,
        },
    ];
    // Nothing lands on the machine now; the retired paths still reconcile.
    let retired = declared
        .skills
        .retired
        .iter()
        .filter(|old| old.root == RetiredRoot::Machine)
        .map(|old| Mapping {
            source: old.path.clone(),
            target: PathBuf::from("skills"),
            tree: true,
        })
        .collect();
    vec![
        (roots.home.clone(), home, Vec::new()),
        (roots.machine.clone(), Vec::new(), retired),
    ]
}

/// Reconciles one root against the pinned content.
///
/// # Invariant
///
/// A file is never overwritten while it differs from the content that installed
/// it: the last reconciled pin is the merge base, the disk is our side, and a
/// conflict leaves the reader's file and the old pin alone, so the next start
/// repeats that merge. Bytes that equal a commit's blob for their own path are
/// an install of another pin, not an edit, so upstream replaces them.
fn reconcile(
    cache: &Path,
    root: &Path,
    mappings: &[Mapping],
    retired: &[Mapping],
    content: &Content,
    report: &mut Report,
) -> anyhow::Result<()> {
    // The report is shared by both roots, so this root judges only what it adds.
    let conflicts_at = report.conflicts.len();
    let _kept_at = report.kept.len();
    let old = read_pin(root);
    // Paths this root already kept from its reader: still theirs, and no longer
    // holding the pin back.
    let mut kept: Vec<String> = read_kept(root);
    // Already at this pin, so upstream cannot have moved: one file read replaces
    // a `git` per installed file.
    if old.as_deref() == Some(content.commit.as_str()) {
        return Ok(());
    }
    let new = collect(cache, &content.commit, mappings, root);
    // Retired paths come from the old pin only: this one has no such tree.
    let before = match old.as_deref() {
        Some(pin) => {
            let mut everything: Vec<&Mapping> = mappings.iter().chain(retired).collect();
            everything.sort_by_key(|m| m.source.len());
            collect_refs(cache, pin, &everything, root)
        }
        None => Vec::new(),
    };

    for file in &new {
        let Some(theirs) = blob(cache, &content.commit, &file.source) else {
            continue;
        };
        let ours = std::fs::read(&file.dest).ok();
        let base = old
            .as_deref()
            .and_then(|pin| blob(cache, pin, &file.source));
        let settled = kept.contains(&file.source);
        let decision = decide(ours.as_deref(), base.as_deref(), &theirs);
        // A stale base can fall behind the files a newer pin left (see the
        // Invariant), and then "both sides moved" is a lie: ours moved because it
        // *is* an install. Bytes that equal a commit's blob for this path carry no
        // local edit, so merging them invents text -- upstream replaces them.
        // A path the reader already owns is exempt: it is never called an install.
        let reinstalled = !settled
            && matches!(
                decision,
                Decision::Keep | Decision::Merge(_) | Decision::Conflict
            )
            && ours
                .as_deref()
                .is_some_and(|ours| is_a_past_install(cache, &content.commit, &file.source, ours));
        let decision = match decision {
            _ if reinstalled => {
                report.rebased.push(file.source.clone());
                Decision::Update
            }
            other => other,
        };
        match decision {
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
            // A path already recorded as the reader's: reported again, not re-recorded.
            _ if settled => report.kept.push(file.source.clone()),
            Decision::Keep => {
                report.kept.push(file.source.clone());
                kept.push(file.source.clone());
            }
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
                prune_dirs(root, &file.dest);
                report.removed += 1;
            }
            Some(_) => {
                report
                    .kept
                    .push(format!("{} (kept: removed upstream)", file.source));
                if !kept.contains(&file.source) {
                    kept.push(file.source.clone());
                }
            }
            None => {}
        }
    }

    // A conflict holds the base and asks again. A kept file is written down, so
    // it does not freeze the pin.
    if report.conflicts.len() == conflicts_at {
        write_pin(root, &content.commit, &kept)?;
    }
    Ok(())
}

/// Takes away the directories a removal left empty, up to `root`.
///
/// # Invariant
///
/// A directory left behind is a name that still exists, and `agents::link_skills`
/// never replaces a name it finds -- so an empty `<machine>/skills/remote` would
/// shadow the community skill of the same name forever, which is worse than the
/// stale files this removal just took away. `root` itself stays: it is the
/// install's own destination, not a skill.
fn prune_dirs(root: &Path, removed: &Path) {
    let mut dir = removed.parent();
    while let Some(here) = dir {
        if here == root || !here.starts_with(root) {
            return;
        }
        if std::fs::remove_dir(here).is_err() {
            return;
        }
        dir = here.parent();
    }
}

/// One installed file: where it came from and where it sits.
struct Planned {
    source: String,
    dest: PathBuf,
    exec: bool,
}

/// Every pinned file a set of mappings accounts for, as a path under `root`.
fn collect(cache: &Path, commit: &str, mappings: &[Mapping], root: &Path) -> Vec<Planned> {
    let refs: Vec<&Mapping> = mappings.iter().collect();
    collect_refs(cache, commit, &refs, root)
}

/// The same, for a set assembled from more than one list.
fn collect_refs(cache: &Path, commit: &str, mappings: &[&Mapping], root: &Path) -> Vec<Planned> {
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

/// Whether these bytes are a past version of `path` that the pin can reach.
///
/// # Invariant
///
/// Only history *the pin reaches*, never `--all`: a commit the reader made in the
/// cache is not an install this code placed, and treating it as one would replace
/// their own words. Only asked when a root has no pin, which is the one case
/// where the base is unknown -- otherwise every stale file in such a home reads
/// as the reader's and freezes, which is what happened to this machine.
fn is_a_past_install(cache: &Path, pin: &str, path: &str, ours: &[u8]) -> bool {
    const LOOK_BACK: usize = 40;
    let Ok(listed) = Command::new(git())
        .arg("--git-dir")
        .arg(cache)
        .args(["rev-list", pin, "--"])
        .arg(path)
        .output()
    else {
        return false;
    };
    if !listed.status.success() {
        return false;
    }
    String::from_utf8_lossy(&listed.stdout)
        .lines()
        .take(LOOK_BACK)
        .any(|commit| blob(cache, commit, path).is_some_and(|bytes| bytes == ours))
}

/// The pin a root is at: the first line of its pin file.
fn read_pin(root: &Path) -> Option<String> {
    std::fs::read_to_string(root.join(PIN_FILE))
        .ok()
        .and_then(|text| text.lines().next().map(|line| line.trim().to_string()))
        .filter(|line| !line.is_empty())
}

/// The paths this root kept from its reader: recorded under the pin.
///
/// # Invariant
///
/// Without this the pin could never advance past a root holding one file of the
/// reader's own -- and a root with no pin has no merge base, so *every* later
/// update reads as "the reader wrote this" and the content freezes at whatever
/// was installed first. See `docs/notes/server-ship.md`.
fn read_kept(root: &Path) -> Vec<String> {
    std::fs::read_to_string(root.join(PIN_FILE))
        .map(|text| {
            text.lines()
                .skip(1)
                .map(|line| line.trim().to_string())
                .filter(|line| !line.is_empty())
                .collect()
        })
        .unwrap_or_default()
}

fn write_pin(root: &Path, commit: &str, kept: &[String]) -> anyhow::Result<()> {
    std::fs::create_dir_all(root)?;
    let mut text = format!("{commit}\n");
    for path in kept {
        text.push_str(path);
        text.push('\n');
    }
    std::fs::write(root.join(PIN_FILE), text)?;
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
