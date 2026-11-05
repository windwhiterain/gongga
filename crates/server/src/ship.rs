//! What the harness ships, and how a home gets it from the content repository.
//!
//! See `docs/notes/server-ship.md`.

use std::path::{Component, Path, PathBuf};
use std::sync::OnceLock;

use serde::Deserialize;

use crate::fork::{Fork, Sync};

/// Where the content comes from, and which protocol this build reads.
///
/// # Invariant
///
/// No commit is named here: the newest one that speaks `protocol` is installed, so
/// content moves by a restart rather than by a rebuild.
const MANIFEST: &str = include_str!("../ship.toml");

/// The file the content declares itself in, at the commit it is read from.
const DECLARATION: &str = "ship.toml";

/// The file naming the content commit a root was last reconciled against.
const PIN_FILE: &str = ".ship-pin";

/// Where a home keeps the content checkout: editable, and offline after one fetch.
const CHECKOUT: &str = "ship";

/// What an older build kept the content in: a bare cache, adopted into a checkout.
const LEGACY: &str = "ship.git";

/// The bytes this home last installed, which are the base of the next merge.
const BASE: &str = ".ship-base";

#[derive(Debug, Deserialize)]
struct Manifest {
    ship: Link,
}

/// Where the content comes from, and which protocol this build reads.
#[derive(Debug, Clone, Deserialize)]
pub struct Link {
    /// Where to fetch: any git URL, or a local checkout while developing.
    pub url: String,
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
pub struct Declared {
    /// The protocol this content speaks, which is how a commit is chosen.
    pub protocol: u32,
    /// The repository path that becomes a home's `config.toml`.
    pub config: String,
    #[serde(default)]
    skills: SkillRoots,
}

/// One commit of the content, with what it declares.
#[derive(Debug, Clone)]
pub struct Content {
    /// The full sha a home reconciles against, and what `.ship-pin` records.
    pub commit: String,
    /// Whether the repository answered this start.
    pub reachable: bool,
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

/// The repository a home named in its own config, kept until the first lookup.
static NAMED: OnceLock<Option<String>> = OnceLock::new();

/// Tells the lookup which repository this home installs from.
///
/// # Invariant
///
/// Called before the first [`link`]: the resolve is memoized for the process, so a
/// URL arriving after it would be a second content for one server. `lib.rs` calls
/// this straight after the config is read and before anything resolves.
pub fn remember_url(url: Option<String>) {
    let _ = NAMED.set(url);
}

/// The manifest this build carries. A manifest that does not parse is a build bug,
/// not a runtime condition.
///
/// # Invariant
///
/// Three answers, most specific first: `GONGGA_SHIP_URL` (one run, one shell, what a
/// script and a test speak), then `[ship] url` in this home's config (a machine
/// developing the content points it at its own checkout), then the compiled one.
pub fn link() -> &'static Link {
    static LINK: OnceLock<Link> = OnceLock::new();
    LINK.get_or_init(|| {
        let mut link = match toml::from_str::<Manifest>(MANIFEST) {
            Ok(m) => m.ship,
            Err(e) => panic!("crates/server/ship.toml does not parse: {e}"),
        };
        if let Some(url) = NAMED.get().and_then(|url| url.clone()) {
            link.url = url;
        }
        if let Some(url) = std::env::var_os("GONGGA_SHIP_URL") {
            link.url = url.to_string_lossy().into_owned();
        }
        link
    })
}

/// Where content lands: a home owns the config and its skills. The machine's skills
/// are the community clone's.
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
    /// Files the content moved and nobody locally had touched.
    pub updated: usize,
    /// Files whose local edits and content changes merged cleanly.
    pub merged: usize,
    /// Files already identical to the installed content.
    pub current: usize,
    /// Files the content dropped that nobody had edited.
    pub removed: usize,
    /// Files left exactly as they are, because the merge would have had to choose.
    pub conflicts: Vec<String>,
    /// Files the reader authored where the content also has one: not ours to
    /// touch, and no root with one counts reconciled.
    pub kept: Vec<String>,
    /// The content commit the roots are now reconciled against.
    pub pin: String,
    /// Whether the repository answered this start.
    pub reachable: bool,
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
        // The failure is cached too: one answer per start, for every caller.
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

/// The checkout this build installs from, for a home.
pub fn checkout(home: &Path) -> PathBuf {
    home.join(CHECKOUT)
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
    let fork = Fork::at(&checkout(&roots.home));
    // Ask for the object, not for a fetch's exit status: see the note.
    if fork.blob(&content.commit, DECLARATION).is_none() {
        anyhow::bail!(
            "the content commit {} is not in {}, so nothing can be installed from it",
            content.commit,
            fork.dir().display()
        );
    }
    let mut report = Report {
        pin: content.commit.clone(),
        reachable: content.reachable,
        ..Default::default()
    };
    for (tag, root, mappings, retired) in plans(roots, content) {
        let base = roots.home.join(BASE).join(tag);
        reconcile(
            &fork,
            &base,
            &root,
            &mappings,
            &retired,
            content,
            &mut report,
        )?;
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
    let home = &roots().home;
    let content = content(home).ok()?;
    Fork::at(&checkout(home))
        .blob(&content.commit, &content.declared.config)
        .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
}

/// Resolves this home's checkout to the newest commit that speaks this build's
/// protocol.
///
/// # Invariant
///
/// Protocol drift walks *back*, never forward: content that has moved to a protocol
/// this build cannot read still serves this build the last commit it could read.
/// The alternative is a machine that cannot start because somebody else's
/// repository advanced.
fn resolve(home: &Path) -> anyhow::Result<Content> {
    let link = link();
    let fork = Fork::at(&checkout(home));
    adopt(home, link)?;
    let mut reachable = true;
    match fork.ensure(&[link.url.clone()])? {
        Sync::Foreign => anyhow::bail!(
            "{} is not a clone, so it is not the shipped content; move it away and start again",
            fork.dir().display()
        ),
        // A fresh clone is at the remote's tip, with the personal branch in it.
        Sync::Cloned(_) => {}
        _ => {
            // Best effort: being offline is not being broken, and a checkout that
            // already holds readable content answers for itself.
            if let Err(e) = fork.sync() {
                tracing::warn!(error = %e, "cannot refresh the shipped content; using what is here");
                reachable = false;
            }
        }
    }
    let tip = fork.content().ok_or_else(|| {
        anyhow::anyhow!(
            "{} holds no branch to take content from, so protocol {} cannot be installed",
            fork.dir().display(),
            link.protocol
        )
    })?;
    let commit = newest_for(&fork, &tip, link)?;
    let declared = declared_at(&fork, &commit)?;
    Ok(Content {
        commit,
        reachable,
        declared,
    })
}

/// Turns the bare cache an older build kept into this home's checkout.
///
/// # Invariant
///
/// An upgrade must not need the network: the cache already holds every commit it
/// did, so a home installed before this build can still start on a machine that is
/// offline, and the fetch below only adds what is newer.
fn adopt(home: &Path, link: &Link) -> anyhow::Result<()> {
    let dir = checkout(home);
    let old = home.join(LEGACY);
    if dir.exists() || !old.is_dir() {
        return Ok(());
    }
    std::fs::create_dir_all(&dir)?;
    std::fs::rename(&old, dir.join(".git"))?;
    crate::fork::git_ok(&dir, &["config", "core.bare", "false"])?;
    if crate::fork::git(&dir, &["remote", "get-url", "origin"])
        .status
        .success()
    {
        crate::fork::git_ok(&dir, &["remote", "set-url", "origin", &link.url])?;
    } else {
        crate::fork::git_ok(&dir, &["remote", "add", "origin", &link.url])?;
    }
    // Best effort: the old refspec named `refs/heads/ship/*`.
    let _ = crate::fork::git(
        &dir,
        &[
            "fetch",
            "-q",
            "origin",
            "+refs/heads/*:refs/remotes/origin/*",
        ],
    );
    let has = |name: &str| {
        crate::fork::git(&dir, &["rev-parse", "--verify", "-q", name])
            .status
            .success()
    };
    let official = crate::fork::OFFICIAL;
    let source = if has(&format!("refs/heads/{official}")) {
        None
    } else if has(&format!("refs/heads/ship/{official}")) {
        Some(format!("refs/heads/ship/{official}"))
    } else if has(&format!("refs/remotes/origin/{official}")) {
        Some(format!("refs/remotes/origin/{official}"))
    } else {
        anyhow::bail!(
            "{} holds no {official} to adopt, so this home has no content to install",
            old.display()
        );
    };
    if let Some(source) = source {
        crate::fork::git_ok(&dir, &["branch", "-f", official, &source])?;
    }
    crate::fork::git_ok(&dir, &["checkout", "-q", "-f", official])?;
    tracing::info!(checkout = %dir.display(), "the old content cache is now a checkout");
    Ok(())
}

/// The newest commit from `tip` back, following first parents, that speaks the
/// protocol this build reads.
fn newest_for(fork: &Fork, tip: &str, link: &Link) -> anyhow::Result<String> {
    // The common case is the tip itself: one read instead of a walk.
    if protocol_at(fork, tip)? == Some(link.protocol) {
        return Ok(tip.to_string());
    }
    let out = crate::fork::git(fork.dir(), &["rev-list", "--first-parent", tip]);
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
        if protocol_at(fork, commit)? == Some(link.protocol) {
            return Ok(commit.to_string());
        }
    }
    anyhow::bail!(
        "no commit on {} speaks protocol {}; the newest one speaks {} and this build reads \
         {}. Build a server that reads the new protocol, or publish content for this one",
        crate::fork::OFFICIAL,
        link.protocol,
        protocol_at(fork, tip)?
            .map(|p| p.to_string())
            .unwrap_or_else(|| "nothing this build can parse".to_string()),
        link.protocol
    )
}

/// The protocol one commit declares, when it declares one this build can parse.
fn protocol_at(fork: &Fork, commit: &str) -> anyhow::Result<Option<u32>> {
    let Some(bytes) = fork.blob(commit, DECLARATION) else {
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
fn declared_at(fork: &Fork, commit: &str) -> anyhow::Result<Declared> {
    let bytes = fork.blob(commit, DECLARATION).ok_or_else(|| {
        anyhow::anyhow!(
            "commit {commit} of {} does not carry {DECLARATION}, so it cannot say where its \
             content belongs",
            link().url
        )
    })?;
    toml::from_str(&String::from_utf8_lossy(&bytes))
        .map_err(|e| anyhow::anyhow!("{DECLARATION} at {commit} does not parse: {e}"))
}

/// Each root's mappings, old and new, tagged by where its merge base lives.
fn plans(
    roots: &Roots,
    content: &Content,
) -> Vec<(&'static str, PathBuf, Vec<Mapping>, Vec<Mapping>)> {
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
        ("home", roots.home.clone(), home, Vec::new()),
        ("machine", roots.machine.clone(), Vec::new(), retired),
    ]
}

/// Reconciles one root against the content.
///
/// # Invariant
///
/// A file is never overwritten while it differs from the content that installed
/// it: the bytes this root last installed are the merge base, the disk is our side,
/// and a conflict leaves the reader's file and the base alone, so the next start
/// repeats that merge.
fn reconcile(
    fork: &Fork,
    base_dir: &Path,
    root: &Path,
    mappings: &[Mapping],
    retired: &[Mapping],
    content: &Content,
    report: &mut Report,
) -> anyhow::Result<()> {
    // The report is shared by both roots, so this root judges only what it adds.
    let conflicts_at = report.conflicts.len();
    let old = read_pin(root);
    // Paths this root already kept from its reader: still theirs, and no longer
    // holding the content back.
    let mut kept: Vec<String> = read_kept(root);
    // Already at this content, so nothing upstream can have moved: one file read
    // replaces a `git` per installed file.
    if old.as_deref() == Some(content.commit.as_str()) {
        return Ok(());
    }
    // An older build's home has a pin and no base: the pin's tree is the base.
    seed_base(fork, base_dir, root, mappings, retired, old.as_deref())?;
    let new = collect(fork, &content.commit, mappings, root);
    // Retired paths come from the old content only: this one has no such tree.
    let before = match old.as_deref() {
        Some(pin) => {
            let mut everything: Vec<&Mapping> = mappings.iter().chain(retired).collect();
            everything.sort_by_key(|m| m.source.len());
            collect_refs(fork, pin, &everything, root)
        }
        None => Vec::new(),
    };

    for file in &new {
        let Some(theirs) = fork.blob(&content.commit, &file.source) else {
            continue;
        };
        let ours = std::fs::read(&file.dest).ok();
        let base = std::fs::read(base_dir.join(&file.rel)).ok();
        let settled = kept.contains(&file.source);
        let decision = decide(ours.as_deref(), base.as_deref(), &theirs);
        match decision {
            Decision::Place => {
                put(&file.dest, &theirs, file.exec)?;
                remember(base_dir, &file.rel, &theirs)?;
                report.written += 1;
            }
            Decision::Update => {
                put(&file.dest, &theirs, file.exec)?;
                remember(base_dir, &file.rel, &theirs)?;
                report.updated += 1;
            }
            Decision::Adopt => {
                put(&file.dest, &theirs, file.exec)?;
                remember(base_dir, &file.rel, &theirs)?;
                report.current += 1;
            }
            Decision::Merge(text) => {
                put(&file.dest, &text, file.exec)?;
                remember(base_dir, &file.rel, &text)?;
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
        if new.iter().any(|planned| planned.source == file.source) {
            continue;
        }
        let base = std::fs::read(base_dir.join(&file.rel)).ok();
        match (std::fs::read(&file.dest).ok(), base) {
            (Some(ours), Some(base)) if ours == base => {
                let _ = std::fs::remove_file(&file.dest);
                let _ = std::fs::remove_file(base_dir.join(&file.rel));
                prune_dirs(root, &file.dest);
                report.removed += 1;
            }
            (Some(_), _) => {
                report
                    .kept
                    .push(format!("{} (kept: removed upstream)", file.source));
                if !kept.contains(&file.source) {
                    kept.push(file.source.clone());
                }
            }
            (None, _) => {}
        }
    }

    // A conflict holds the base and asks again. A kept file is written down, so
    // it does not freeze the content.
    if report.conflicts.len() == conflicts_at {
        write_pin(root, &content.commit, &kept)?;
    }
    Ok(())
}

/// Copies the bytes the pin names into the base directory, once.
///
/// # Invariant
///
/// Only when this root has no base of its own: a base that exists is what the last
/// install placed, and re-seeding it from a commit would silently adopt that commit
/// as the truth for every local edit.
fn seed_base(
    fork: &Fork,
    base_dir: &Path,
    root: &Path,
    mappings: &[Mapping],
    retired: &[Mapping],
    pin: Option<&str>,
) -> anyhow::Result<()> {
    let Some(pin) = pin else {
        return Ok(());
    };
    if base_dir.is_dir() {
        return Ok(());
    }
    let mut everything: Vec<&Mapping> = mappings.iter().chain(retired).collect();
    everything.sort_by_key(|m| m.source.len());
    for file in collect_refs(fork, pin, &everything, root) {
        let Some(bytes) = fork.blob(pin, &file.source) else {
            continue;
        };
        remember(base_dir, &file.rel, &bytes)?;
    }
    Ok(())
}

/// Records what was placed at one path, which is the base of the next merge.
fn remember(base_dir: &Path, rel: &Path, bytes: &[u8]) -> anyhow::Result<()> {
    put(&base_dir.join(rel), bytes, false)
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
    /// The path under the root, which is also its path under the base directory.
    rel: PathBuf,
    dest: PathBuf,
    exec: bool,
}

/// Every installed file a set of mappings accounts for, as a path under `root`.
fn collect(fork: &Fork, commit: &str, mappings: &[Mapping], root: &Path) -> Vec<Planned> {
    let refs: Vec<&Mapping> = mappings.iter().collect();
    collect_refs(fork, commit, &refs, root)
}

/// The same, for a set assembled from more than one list.
fn collect_refs(fork: &Fork, commit: &str, mappings: &[&Mapping], root: &Path) -> Vec<Planned> {
    let mut out = Vec::new();
    for mapping in mappings {
        for (path, exec) in listed(fork, commit, &mapping.source) {
            let rel = if mapping.tree {
                let Some(rel) = under(&mapping.source, &path).and_then(|r| safe_relpath(&r)) else {
                    tracing::warn!(path = %path, "an installed path is not installable; skipped");
                    continue;
                };
                mapping.target.join(rel)
            } else {
                mapping.target.clone()
            };
            out.push(Planned {
                source: path,
                dest: root.join(&rel),
                rel,
                exec,
            });
        }
    }
    out
}

/// The files one repository path holds at a commit, which may be a file or a tree.
fn listed(fork: &Fork, commit: &str, path: &str) -> Vec<(String, bool)> {
    let out = crate::fork::git(fork.dir(), &["ls-tree", "-r", "--long", commit, "--", path]);
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
    /// Nothing local claims it: place the content.
    Place,
    /// The content moved it and the local copy still matched what installed it.
    Update,
    /// Identical already, so only the executable bit may need settling.
    Adopt,
    /// Both sides moved and git joined them.
    Merge(Vec<u8>),
    /// The reader's file stands, and the root keeps its base.
    Conflict,
    /// A file the reader made at a path the content also has: not ours to touch.
    Keep,
}

fn decide(ours: Option<&[u8]>, base: Option<&[u8]>, theirs: &[u8]) -> Decision {
    match (ours, base) {
        (None, _) => Decision::Place,
        (Some(ours), _) if ours == theirs => Decision::Adopt,
        // No merge base: the reader wrote this file, and the content has one at the
        // same path. Neither may overwrite the other.
        (Some(_), None) => Decision::Keep,
        (Some(ours), Some(base)) if ours == base => Decision::Update,
        (Some(ours), Some(base)) => match merge_text(ours, base, theirs) {
            Some(merged) => Decision::Merge(merged),
            None => Decision::Conflict,
        },
    }
}

/// `git merge-file` across three temp files; `None` is a conflict or a failure, and
/// both keep the reader's file.
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
    let merged = crate::fork::bounded()
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
    let normal = Path::new(path);
    if normal.is_absolute() {
        return None;
    }
    for part in normal.components() {
        match part {
            Component::Normal(name) => out.push(name),
            // A drive letter, a root and a `..` are all "somewhere else".
            Component::Prefix(_) | Component::RootDir | Component::ParentDir => return None,
            Component::CurDir => {}
        }
    }
    (!out.as_os_str().is_empty()).then_some(out)
}

/// The pin a root was last reconciled against.
fn read_pin(root: &Path) -> Option<String> {
    let text = std::fs::read_to_string(root.join(PIN_FILE)).ok()?;
    text.lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .map(str::to_string)
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
    std::fs::write(root.join(PIN_FILE), text)
        .map_err(|e| anyhow::anyhow!("recording the content in {}: {e}", root.display()))
}

/// Writes a file whole, keeping the mode the repository recorded for it.
fn put(dest: &Path, bytes: &[u8], exec: bool) -> anyhow::Result<()> {
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(dest, bytes).map_err(|e| anyhow::anyhow!("writing {}: {e}", dest.display()))?;
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
