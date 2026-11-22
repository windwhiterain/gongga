//! What the harness ships, and how a home reads it from the content repository.
//!
//! See `docs/notes/server-ship.md`.

use std::path::{Component, Path, PathBuf};
use std::sync::{Mutex, OnceLock, RwLock, mpsc};

use serde::Deserialize;

use crate::fork::{Fork, Sync};

/// Where the content comes from, and which protocol this build reads.
///
/// # Invariant
///
/// No commit is named here: the newest one that speaks `protocol` is read, so content
/// moves by a restart rather than by a rebuild.
const MANIFEST: &str = include_str!("../ship.toml");

/// The file the content declares itself in, at the commit it is read from.
const DECLARATION: &str = "ship.toml";

/// Where a home keeps the content checkout: the files it reads, and nothing else.
const CHECKOUT: &str = "ship";

/// What an older build kept the content in: a bare cache, adopted into a checkout.
const LEGACY: &str = "ship.git";

/// What an older build merged its own copies against; this build keeps no copies.
const BASE: &str = ".ship-base";

/// The commit those copies came from, recorded beside them.
const PIN_FILE: &str = ".ship-pin";

/// Where an older build installed the content's config in the home.
const OLD_CONFIG: &str = "config.toml";

/// Where it installed the content's skills.
const OLD_SKILLS: &str = "skills";

/// Where it installed the Python surface.
const OLD_LIB: &str = "lib";

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

/// What the content says about itself: which protocol it is, and where its parts sit
/// in the checkout a home reads.
///
/// # Invariant
///
/// The mapping is content, not code. A retired path or a renamed skills root is a
/// fact about this repository's own history, and keeping it in the binary would mean a
/// rebuild for exactly the kind of edit this design exists to make free.
#[derive(Debug, Clone, Deserialize)]
pub struct Declared {
    /// The protocol this content speaks, which is how a commit is chosen.
    pub protocol: u32,
    /// The repository path that is a home's config.
    pub config: String,
    /// The repository path whose files are the Python surface.
    pub python: String,
    #[serde(default)]
    skills: SkillRoots,
    /// The commands this content adds to a home's catalog, in the order it declares them.
    ///
    /// # Invariant
    ///
    /// Absent from a commit that declares none, which is why the list defaults: the
    /// mapping a protocol-12 commit carries is still this build's to read. The key is
    /// `[[command]]` -- one table per command, the way a TOML declares a list.
    #[serde(default, rename = "command")]
    pub commands: Vec<Command>,
}

/// One command the content declares: a function of one of its skills, published under a
/// name of its own.
///
/// # Invariant
///
/// Three words and no arguments. What a call carries is that function's signature, which
/// the contract answers when a manifest is asked for, so a declaration that listed
/// arguments too would be a second answer to a question Python already answers.
#[derive(Debug, Clone, Deserialize)]
pub struct Command {
    /// The name the command is published and called by.
    pub name: String,
    /// The id of the skill that implements it, as `ls_skill` prints one.
    pub skill: String,
    /// The function of that package to run.
    pub function: String,
}

/// One commit of the content, with what it declares.
#[derive(Debug, Clone)]
pub struct Content {
    /// The full sha a home reads, and the commit log lines name.
    pub commit: String,
    declared: Declared,
}

impl Content {
    /// The commands this commit declares; empty for one that declares none.
    pub fn commands(&self) -> &[Command] {
        &self.declared.commands
    }
}

/// Where each bucket of skills lives in the content.
#[derive(Debug, Clone, Default, Deserialize)]
struct SkillRoots {
    /// Where this repository keeps the skills it owns, one dir per skill.
    harness: String,
    /// Paths an older commit installed on the machine and this one does not, so their
    /// copies go.
    ///
    /// # Invariant
    ///
    /// Each one still names the path it was installed at, because that is where its
    /// files have to be found to be recognised as ours.
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

/// The manifest this build carries. A manifest that does not parse is a build bug,
/// not a runtime condition.
///
/// # Invariant
///
/// Two answers, most specific first: `GONGGA_SHIP_URL` (one run, one shell, what a
/// script and a test speak), then the compiled one. A home may name its own repository
/// as well, in its config; that one is applied where the fetch happens
/// (`follow`), because the config lives *inside* the checkout it would have to be
/// known before.
pub fn link() -> &'static Link {
    static LINK: OnceLock<Link> = OnceLock::new();
    LINK.get_or_init(|| {
        let mut link = match toml::from_str::<Manifest>(MANIFEST) {
            Ok(manifest) => manifest.ship,
            Err(e) => panic!("crates/server/ship.toml does not parse: {e}"),
        };
        if let Some(url) = std::env::var_os("GONGGA_SHIP_URL") {
            link.url = url.to_string_lossy().into_owned();
        }
        link
    })
}

/// Where this home's checkout fetches from: the repository its config names, else the
/// one this build carries.
fn url(named: Option<&str>) -> String {
    named
        .map(str::to_string)
        .unwrap_or_else(|| link().url.clone())
}

/// Where a home reads the content, and which commit it is reading.
///
/// # Invariant
///
/// Paths into the checkout, never copies: the files a home reads are the content's own,
/// so an edit there is an edit here, with nothing to install and nothing to converge.
#[derive(Debug, Clone)]
pub struct Paths {
    /// The file this home's config is: `<checkout>/<declared config>`.
    pub config: PathBuf,
    /// The directory holding one directory per skill: `<checkout>/<declared skills>`.
    pub skills: PathBuf,
    /// What `GONGGA_SKILL_LIB` names: `<checkout>/<declared python>`.
    pub python: PathBuf,
    /// The content commit the checkout is on.
    pub commit: String,
}

/// Where the machine's own skills are: the community clone's bucket.
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
        .map(|home| home.join(".agents"))
        .unwrap_or_else(|| PathBuf::from(".agents"))
}

/// The live answer; `None` until the first resolve.
static CONTENT: RwLock<Option<Result<Content, String>>> = RwLock::new(None);

/// The one fetch a start makes, and its answer, shared by resolve and refresh.
static FETCH: OnceLock<Mutex<Option<mpsc::Receiver<Result<Sync, String>>>>> = OnceLock::new();
static FETCHED: OnceLock<Result<Sync, String>> = OnceLock::new();
/// The content this process reads, re-resolved when the checkout moves.
///
/// # Invariant
///
/// Live tracked: a resolve reads the tip as it is, and a fetch that moved it resolves
/// again. One call gets one `Content`, so no reader holds a reference into state another
/// thread may replace.
pub fn content(home: &Path) -> anyhow::Result<Content> {
    let held = CONTENT.read().unwrap_or_else(|e| e.into_inner());
    if let Some(cached) = held.as_ref() {
        return cached.clone().map_err(|e| anyhow::anyhow!("{e}"));
    }
    drop(held);
    let fresh = resolve(home).map_err(|e| format!("{e:#}"));
    let mut held = CONTENT.write().unwrap_or_else(|e| e.into_inner());
    held.get_or_insert(fresh)
        .clone()
        .map_err(|e| anyhow::anyhow!("{e}"))
}

/// The commit this process reads, when it resolved one.
pub fn resolved_commit() -> Option<String> {
    CONTENT
        .read()
        .unwrap_or_else(|e| e.into_inner())
        .as_ref()
        .and_then(|resolved| resolved.as_ref().ok())
        .map(|content| content.commit.clone())
}

/// Replaces the live answer after the checkout has been moved to it.
fn set_content(fresh: Content) {
    *CONTENT.write().unwrap_or_else(|e| e.into_inner()) = Some(Ok(fresh));
}

/// The checkout this build reads from, for a home.
pub fn checkout(home: &Path) -> PathBuf {
    home.join(CHECKOUT)
}

/// Where this home's own skills are read: the content's tree, in place.
pub fn skills(home: &Path) -> PathBuf {
    mapped(home, |declared| &declared.skills.harness, "skills")
}

/// What `GONGGA_SKILL_LIB` names: the content's Python surface, in place.
pub fn python(home: &Path) -> PathBuf {
    mapped(home, |declared| &declared.python, "python")
}

/// The file this home's config is.
pub fn config(home: &Path) -> PathBuf {
    mapped(home, |declared| &declared.config, "config/config.toml")
}

/// One path the content declares, under the checkout.
///
/// # Invariant
///
/// The declaration is read while the content resolves, so a home that never resolved
/// answers with the name this mapping has always had: nothing can be copied out of a
/// content that was never read, and a path that is wrong is refused by `paths_of`
/// before any of it is used.
fn mapped(home: &Path, of: fn(&Declared) -> &str, otherwise: &str) -> PathBuf {
    let declared = content(home)
        .map(|content| of(&content.declared).to_string())
        .unwrap_or_else(|_| otherwise.to_string());
    let rel = safe_relpath(&declared).unwrap_or_else(|| PathBuf::from(otherwise));
    checkout(home).join(rel)
}

/// The start's first half: this home's checkout, on the commit this build reads.
///
/// # Invariant
///
/// Nothing is copied into the home. The checkout is cloned if the home has none, moved
/// to the commit this build reads (usually it is already there, and no git runs), and
/// from then on every reader opens the checkout's own files.
pub fn prepare(home: &Path) -> anyhow::Result<Paths> {
    let _ = ROOTS.set(Roots::of(home));
    let fork = Fork::at(&checkout(home));
    let named = url(None);
    adopt(home, &named)?;
    if !fork.exists() {
        match fork.ensure(&[named.clone()])? {
            Sync::Foreign => anyhow::bail!(
                "{} is not a clone, so it is not the shipped content; move it away and \
                 start again",
                fork.dir().display()
            ),
            _ => tracing::info!("content: this home had no checkout, and one is cloned into it"),
        }
    }
    let content = content(home)?;
    settle(&fork, &content.commit)?;
    let paths = paths_of(&fork, &content)?;
    tidy(home, &paths, &content);
    tracing::info!(
        content = %paths.commit.get(..10).unwrap_or(paths.commit.as_str()),
        config = %paths.config.display(),
        skills = %paths.skills.display(),
        python = %paths.python.display(),
        "content: this home reads the checkout in place"
    );
    Ok(paths)
}

/// The same, for a question: read what this home holds, and write nothing.
///
/// # Invariant
///
/// A handover asks this with the successor's binary *before* it cancels the server the
/// config belongs to, so no clone, no fetch and no move happens here: a checkout that
/// speaks no protocol this build reads answers with its own refusal, and nothing that
/// was there has changed.
pub fn inspect(home: &Path) -> anyhow::Result<Paths> {
    let fork = Fork::at(&checkout(home));
    if !fork.exists() {
        anyhow::bail!(
            "this home reads its content from {}, and there is no checkout there",
            fork.dir().display()
        );
    }
    let tip = fork.content().ok_or_else(|| {
        anyhow::anyhow!(
            "{} holds no branch to take content from",
            fork.dir().display()
        )
    })?;
    let link = link();
    let commit = newest_for(&fork, &tip, link)?;
    let declared = declared_at(&fork, &commit)?;
    let content = Content { commit, declared };
    paths_of(&fork, &content)
}

/// Points this home's checkout at the repository its config names, and starts the one
/// fetch this start makes.
///
/// # Invariant
///
/// The repository is the home's answer, not a property of the clone: a home whose
/// config moved from the published content to a local checkout fetches from there at
/// this start, and moving back is the same edit removed. Only `origin` moves -- the
/// commits, the personal branch and the working tree are this home's own. One fetch per
/// process, reused by every later `refresh`.
pub fn follow(home: &Path, named: Option<&str>) {
    let url = url(named);
    let fork = Fork::at(&checkout(home));
    let (tx, rx) = mpsc::sync_channel(1);
    if FETCH.set(Mutex::new(Some(rx))).is_err() {
        return;
    }
    if point(home, &fork, &url).is_err() || !fork.exists() {
        // `prepare` reported what is wrong with this checkout; nothing to fetch.
        let _ = tx.send(Ok(Sync::Current));
        return;
    }
    let started = std::thread::Builder::new()
        .name("ship-fetch".to_string())
        .spawn(move || {
            let _ = tx.send(fork.sync().map_err(|e| format!("{e:#}")));
        });
    if let Err(e) = started {
        tracing::warn!(error = %e, "the content fetch could not start");
    }
}

/// The answer of that fetch, waiting for it if it has not finished.
fn fetched() -> &'static Result<Sync, String> {
    FETCHED.get_or_init(|| {
        let Some(slot) = FETCH.get() else {
            return Err("the content fetch was never started".to_string());
        };
        let mut held = slot.lock().unwrap_or_else(|e| e.into_inner());
        let Some(rx) = held.take() else {
            return Err("the content fetch was already taken".to_string());
        };
        rx.recv()
            .unwrap_or_else(|_| Err("the content fetch ended without an answer".to_string()))
    })
}

/// Waits for this start's fetch, for the one caller that cannot go on without it.
///
/// # Invariant
///
/// Only a home behind this build asks: a migration step can exist in the fetched
/// commit and nowhere in the pin, and a home that cannot start at all is owed the
/// wait. Every other start reads what it has and lets `refresh` move the checkout
/// behind the port. The answer is the caller's to log, never to abort on. See
/// `docs/notes/server-migrate.md`.
pub fn await_fetch() -> Result<Sync, String> {
    fetched().clone()
}

/// Resolves the moved checkout again and moves the roots' readers onto it.
///
/// # Invariant
///
/// Live tracked: `resolve` reads the tip as it is, the checkout is moved to the commit
/// it names, and the live answer moves only after that. A start with nothing new runs
/// no git; protocol drift still walks back.
pub fn refresh(home: &Path) -> anyhow::Result<Option<String>> {
    let fork = Fork::at(&checkout(home));
    if !fork.exists() {
        return Ok(None);
    }
    let state = fetched()
        .as_ref()
        .map_err(|why| anyhow::anyhow!("the content fetch failed: {why}"))?;
    if *state != Sync::Fetched {
        return Ok(None);
    }
    let fresh = resolve(home)?;
    // Settled before the comparison: a fetch can move the tree, and not the commit.
    settle(&fork, &fresh.commit)?;
    if fresh.commit == content(home)?.commit {
        return Ok(None);
    }
    let commit = fresh.commit.clone();
    set_content(fresh);
    Ok(Some(commit))
}

/// Reads this home's checkout and names the commit this build reads from it.
///
/// # Invariant
///
/// Protocol drift walks *back*, never forward: content that has moved to a protocol
/// this build cannot read still serves this build the last commit it could read. The
/// alternative is a machine that cannot start because somebody else's repository
/// advanced. A checkout whose history holds nothing readable waits for the fetch this
/// start already made, then walks the history it brought.
fn resolve(home: &Path) -> anyhow::Result<Content> {
    let fork = Fork::at(&checkout(home));
    let link = link();
    let tip = fork.content().ok_or_else(|| {
        anyhow::anyhow!(
            "{} holds no branch to take content from, so protocol {} cannot be read",
            fork.dir().display(),
            link.protocol
        )
    })?;
    let commit = match newest_for(&fork, &tip, link) {
        Ok(commit) => commit,
        Err(why) => wait_for_fetch(&fork, &tip, link, why)?,
    };
    let declared = declared_at(&fork, &commit)?;
    Ok(Content { commit, declared })
}

/// Waits for the fetch this start already made when the checkout's own history says
/// nothing this build can read.
///
/// # Invariant
///
/// The fetch is shared, so waiting costs no second round trip; one resolve still names
/// one commit. `refresh` runs behind the port, which is why a checkout the fetch was
/// about to move can still be read: without it a build whose content moved would never
/// reach the port where the fetch is collected.
fn wait_for_fetch(
    fork: &Fork,
    tip: &str,
    link: &Link,
    first: anyhow::Error,
) -> anyhow::Result<String> {
    let state = match fetched() {
        Ok(state) => state.clone(),
        Err(why) => anyhow::bail!("{first:#}; the content fetch this start makes: {why}"),
    };
    let after = fork.content().unwrap_or_else(|| tip.to_string());
    if after == tip {
        anyhow::bail!(
            "{first:#}; the content fetch left the checkout at {tip} ({state:?}). A personal \
             branch that did not rebase keeps its own history: resolve that rebase before starting again"
        );
    }
    newest_for(fork, &after, link)
        .map_err(|again| anyhow::anyhow!("{first:#}; after the content fetch: {again:#}"))
}

/// Where this home reads the content this commit describes, refusing a checkout that
/// cannot serve one part of it.
///
/// # Invariant
///
/// A home is no longer self-contained: its prose, its skills and its Python surface are
/// the checkout's files. So a checkout missing any of them is a refusal that names the
/// path -- never a server that starts and cannot say where its config went.
fn paths_of(fork: &Fork, content: &Content) -> anyhow::Result<Paths> {
    let declared = &content.declared;
    let config = inside(fork, &declared.config, "config")?;
    let skills = inside(fork, &declared.skills.harness, "skills")?;
    let python = inside(fork, &declared.python, "python")?;
    for (what, at) in [
        ("config", &config),
        ("skills", &skills),
        ("python", &python),
    ] {
        if std::fs::symlink_metadata(at).is_err() {
            anyhow::bail!(
                "the content at {} declares its {what} at {}, and nothing is there: this \
                 home has no {what} to read",
                content.commit,
                at.display()
            );
        }
    }
    Ok(Paths {
        config,
        skills,
        python,
        commit: content.commit.clone(),
    })
}

/// One declared path, under the checkout, refusing one that climbs out of it.
fn inside(fork: &Fork, declared: &str, what: &str) -> anyhow::Result<PathBuf> {
    let rel = safe_relpath(declared).ok_or_else(|| {
        anyhow::anyhow!(
            "the content declares its {what} at {declared:?}, which is not a path inside the \
             checkout {}",
            fork.dir().display()
        )
    })?;
    Ok(fork.dir().join(rel))
}

/// Puts the checkout's working tree on the commit this build reads.
///
/// # Invariant
///
/// Nothing is copied, so the checkout *is* what a home reads, and it has to stand on the
/// commit the walk-back named. A start that finds it already there runs no git; when it
/// does not, git moves the tree, onto the branch that names that commit or detached when
/// none does. Work in the tree that would have to go is a refusal: it is the reader's.
fn settle(fork: &Fork, commit: &str) -> anyhow::Result<()> {
    if fork.head().as_deref() == Some(commit) {
        return Ok(());
    }
    let branch = [crate::fork::PERSONAL, crate::fork::OFFICIAL]
        .into_iter()
        .find(|name| fork.branch_commit(name).as_deref() == Some(commit));
    let out = crate::fork::git(fork.dir(), &["checkout", "-q", branch.unwrap_or(commit)]);
    if !out.status.success() {
        anyhow::bail!(
            "the checkout {} is not on commit {commit}, which is the content this build reads, \
             and moving it there failed: {}",
            fork.dir().display(),
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    tracing::info!(
        checkout = %fork.dir().display(),
        commit = %commit,
        branch = ?branch,
        "content: the checkout moved to the commit this build reads"
    );
    Ok(())
}

/// Turns the bare cache an older build kept into this home's checkout.
///
/// # Invariant
///
/// An upgrade must not need the network: the cache already holds every commit it did,
/// so a home installed before this build can still start on a machine that is offline.
fn adopt(home: &Path, url: &str) -> anyhow::Result<()> {
    let dir = checkout(home);
    let old = home.join(LEGACY);
    if Fork::at(&dir).exists() || !old.is_dir() {
        return Ok(());
    }
    std::fs::create_dir_all(&dir)?;
    std::fs::rename(&old, dir.join(".git"))?;
    // An explicit git dir: the cache is bare, and a relative question would be
    // answered by the repository above this home.
    let git_dir = dir.join(".git");
    let named = crate::fork::git_at(&git_dir, None, &["rev-parse", "--git-dir"]);
    if !named.status.success() {
        let _ = std::fs::rename(&git_dir, &old);
        anyhow::bail!(
            "{} is not a repository, so it is not this home's content; left where it is",
            old.display()
        );
    }
    crate::fork::git_ok(&dir, &["config", "core.bare", "false"])?;
    // Converted, it must answer as this directory's own checkout -- see `Fork::exists`.
    if !Fork::at(&dir).exists() {
        anyhow::bail!(
            "{} was adopted but is not a checkout of this home",
            dir.display()
        );
    }
    if crate::fork::git(&dir, &["remote", "get-url", "origin"])
        .status
        .success()
    {
        crate::fork::git_ok(&dir, &["remote", "set-url", "origin", url])?;
    } else {
        crate::fork::git_ok(&dir, &["remote", "add", "origin", url])?;
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
            "{} holds no {official} to adopt, so this home has no content to read",
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

/// Points an existing checkout's `origin` at the repository this home names.
///
/// # Invariant
///
/// The commits, the personal branch and the working tree stay this home's own; only
/// where the next fetch reads from moves. Nothing is written when it already agrees.
fn point(home: &Path, fork: &Fork, url: &str) -> anyhow::Result<()> {
    if !fork.exists() {
        return Ok(());
    }
    let current = crate::fork::git(fork.dir(), &["remote", "get-url", "origin"]);
    let named = String::from_utf8_lossy(&current.stdout).trim().to_string();
    if current.status.success() && named == url {
        return Ok(());
    }
    tracing::info!(
        home = %home.display(),
        from = %named,
        to = %url,
        "content: the checkout's origin follows this home's config"
    );
    crate::fork::git_ok(fork.dir(), &["remote", "set-url", "origin", url])
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
            fork.dir().display(),
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    let mut walked = 0usize;
    for commit in String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
    {
        walked += 1;
        if protocol_at(fork, commit)? == Some(link.protocol) {
            return Ok(commit.to_string());
        }
    }
    let why = match declared_at(fork, tip) {
        Ok(declared) => format!("speaks {}", declared.protocol),
        Err(e) => format!("does not parse: {e}"),
    };
    tracing::warn!(
        commit = %tip,
        walked,
        "walked the history and found no declaration of this build's protocol: the newest one {why}"
    );
    anyhow::bail!(
        "no commit on {} speaks protocol {}; the newest one {} and this build reads {}. Build \
         a server that reads the new protocol, or publish content for this one",
        crate::fork::OFFICIAL,
        link.protocol,
        why,
        link.protocol
    )
}

/// The protocol one commit declares, when it declares one this build can parse.
fn protocol_at(fork: &Fork, commit: &str) -> anyhow::Result<Option<u32>> {
    let Some(bytes) = fork.blob(commit, DECLARATION) else {
        return Ok(None);
    };
    Ok(toml::from_str::<Declared>(&String::from_utf8_lossy(&bytes))
        .ok()
        .map(|declared| declared.protocol))
}

/// What one commit declares about itself.
fn declared_at(fork: &Fork, commit: &str) -> anyhow::Result<Declared> {
    let bytes = fork.blob(commit, DECLARATION).ok_or_else(|| {
        anyhow::anyhow!(
            "commit {commit} of {} does not carry {DECLARATION}, so it cannot say where its \
             content belongs",
            fork.dir().display()
        )
    })?;
    toml::from_str(&String::from_utf8_lossy(&bytes))
        .map_err(|e| anyhow::anyhow!("{DECLARATION} at {commit} does not parse: {e}"))
}

/// Takes away what an older build copied into this home.
///
/// # Invariant
///
/// Bytes are the test, because provenance is not on disk: a file goes only when it is
/// the content's own copy, and a skill directory only when every file in it is. A copy
/// that differs -- an older protocol's, or one somebody edited -- stays where it is and
/// is named in the log. A name the content never shipped is never looked at at all.
fn tidy(home: &Path, paths: &Paths, content: &Content) {
    // An older build's own three names: not the content's bytes to compare with.
    crate::retired_python::retire(home);
    // The two records an older build merged its copies against: nothing reads them now.
    let (base, pin) = (home.join(BASE), home.join(PIN_FILE));
    let recorded =
        std::fs::symlink_metadata(&base).is_ok() || std::fs::symlink_metadata(&pin).is_ok();
    let _ = std::fs::remove_dir_all(&base);
    let _ = std::fs::remove_file(&pin);
    if recorded {
        tracing::info!(
            home = %home.display(),
            "content: the merge base an older build kept is gone; a home's files are the \
             checkout's own"
        );
    }
    let copy = home.join(OLD_CONFIG);
    if let Ok(found) = std::fs::read(&copy) {
        if same_bytes(&paths.config, &found) {
            let _ = std::fs::remove_file(&copy);
            tracing::info!(
                removed = %copy.display(),
                config = %paths.config.display(),
                "content: the config an older build copied here is gone"
            );
        } else {
            tracing::warn!(
                kept = %copy.display(),
                config = %paths.config.display(),
                "an older build copied a config here and it differs from the content's, so it \
                 is kept and read by nobody: this home's config is the content's own file"
            );
        }
    }
    let lib = home.join(OLD_LIB);
    if let Ok(entries) = std::fs::read_dir(&lib) {
        for entry in entries.flatten() {
            let at = entry.path();
            let Some(name) = at.file_name() else {
                continue;
            };
            let theirs = paths.python.join(name);
            let Ok(found) = std::fs::read(&at) else {
                continue;
            };
            if same_bytes(&theirs, &found) {
                let _ = std::fs::remove_file(&at);
                tracing::info!(removed = %at.display(), "content: the copy an older build put here is gone");
            } else if std::fs::symlink_metadata(&theirs).is_ok() {
                tracing::warn!(
                    kept = %at.display(),
                    python = %paths.python.display(),
                    "an older build put a file here that differs from the content's, so it is \
                     kept and read by nobody: the Python surface is the content's own directory"
                );
            }
        }
        // Only an empty `lib` goes: whatever else is there is somebody's.
        let _ = std::fs::remove_dir(&lib);
    }
    let bucket = home.join(OLD_SKILLS);
    if let Ok(entries) = std::fs::read_dir(&bucket) {
        for entry in entries.flatten() {
            let at = entry.path();
            let Some(name) = at.file_name() else {
                continue;
            };
            let theirs = paths.skills.join(name);
            if !at.is_dir() || std::fs::symlink_metadata(&theirs).is_err() {
                continue;
            }
            if same_tree(&at, &theirs) {
                let _ = std::fs::remove_dir_all(&at);
                tracing::info!(
                    removed = %at.display(),
                    skill = %theirs.display(),
                    "content: the copy of this skill an older build installed is gone"
                );
            } else {
                tracing::warn!(
                    kept = %at.display(),
                    skill = %theirs.display(),
                    "an older build installed this skill here and it differs from the content's, \
                     so it is kept and read by nobody"
                );
            }
        }
        let _ = std::fs::remove_dir(&bucket);
    }
    // A path an earlier commit installed on the machine, which this content dropped.
    for old in content
        .declared
        .skills
        .retired
        .iter()
        .filter(|old| old.root == RetiredRoot::Machine)
    {
        retire(&machine_root(), &old.path);
    }
}

/// Whether a file holds exactly these bytes, which is what makes it a copy of ours.
fn same_bytes(at: &Path, bytes: &[u8]) -> bool {
    std::fs::read(at).ok().as_deref() == Some(bytes)
}

/// Whether two directories hold the same files, byte for byte.
fn same_tree(left: &Path, right: &Path) -> bool {
    match (tree(left, Path::new("")), tree(right, Path::new(""))) {
        (Some(left), Some(right)) => left == right,
        _ => false,
    }
}

/// Every file under `dir`, by its path within it and its bytes, in one order.
///
/// # Invariant
///
/// A directory that cannot be read answers nothing, so it is never equal to another:
/// an unreadable tree is not a copy this install may delete.
fn tree(dir: &Path, under: &Path) -> Option<Vec<(PathBuf, Vec<u8>)>> {
    let mut out = Vec::new();
    for entry in std::fs::read_dir(dir).ok()?.flatten() {
        let rel = under.join(entry.file_name());
        let at = entry.path();
        if at.is_dir() {
            out.extend(tree(&at, &rel)?);
        } else {
            out.push((rel, std::fs::read(&at).ok()?));
        }
    }
    out.sort();
    Some(out)
}

/// Takes away a path an earlier commit installed, which this content no longer has.
///
/// # Invariant
///
/// The directory goes whole, at the path the content named: there is nothing to compare
/// its files with, and a copy left behind shadows the community clone's skill of the same
/// name forever, since `agents::link_skills` never replaces a name it finds. A link goes
/// as a link: the directory behind it is not this install's.
fn retire(root: &Path, path: &str) {
    let Some(rel) = safe_relpath(path) else {
        tracing::warn!(path = %path, "a retired path is not a path under a root; skipped");
        return;
    };
    let at = root.join(rel);
    if std::fs::symlink_metadata(&at).is_err() {
        return;
    }
    let outcome = std::fs::remove_dir(&at)
        .or_else(|_| std::fs::remove_dir_all(&at))
        .or_else(|_| std::fs::remove_file(&at));
    match outcome {
        Ok(()) => tracing::info!(
            removed = %at.display(),
            "content: an earlier commit installed this and the content no longer has it, so \
             its copies go"
        ),
        Err(error) => tracing::warn!(
            path = %at.display(),
            %error,
            "content: a retired path could not be removed"
        ),
    }
}

/// Where one repository path belongs under a root, refusing anything that climbs.
///
/// # Safety
///
/// The path is read out of a repository someone else pushed to, so `..`, an absolute
/// root and a drive prefix are refused rather than joined.
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

/// The roots this process read content from.
static ROOTS: OnceLock<Roots> = OnceLock::new();

/// Where the skills are: the roots of the last resolve, or the home this process would
/// have used if it resolved none.
pub fn roots() -> &'static Roots {
    ROOTS.get_or_init(|| {
        let home = std::env::var_os("GONGGA_HOME")
            .map(PathBuf::from)
            .or_else(|| dirs::home_dir().map(|home| home.join(".gongga")))
            .unwrap_or_else(|| PathBuf::from(".gongga"));
        Roots::of(&home)
    })
}
