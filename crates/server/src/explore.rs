//! SPEC's `set_session --mode`: what a session may change while it reads.
//!
//! # Invariant
//!
//! `Mode::Explore` is a reminder, not a jail: a shell writes whatever the system lets
//! it, so this refuses the tools whose identity is the write and reports the workspace
//! a program changed when its job ends. Nothing is undone -- reverting would delete
//! work that is the operator's. See `docs/notes/server-explore.md`.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::{Arc, Mutex};

use serde_json::json;

use crate::store::Store;
use crate::tools::ToolContext;

/// Whether a session may change what it reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Mode {
    /// The mode of a session that never asked: it may write.
    #[default]
    Work,
    /// It reads: the write-shaped tools are refused and a program's change is reported.
    Explore,
}

impl Mode {
    /// The word a row keeps and a command says.
    pub const WORK: &'static str = "work";
    /// The word that puts the reminder in force.
    pub const EXPLORE: &'static str = "explore";

    pub fn as_str(self) -> &'static str {
        match self {
            Mode::Work => Self::WORK,
            Mode::Explore => Self::EXPLORE,
        }
    }

    /// Reads a command argument: an unknown word is refused, not guessed at.
    pub fn parse(word: &str) -> Result<Mode, String> {
        let word = word.trim();
        if word.is_empty() || word.eq_ignore_ascii_case(Self::WORK) {
            return Ok(Mode::Work);
        }
        if word.eq_ignore_ascii_case(Self::EXPLORE) {
            return Ok(Mode::Explore);
        }
        Err(format!(
            "unknown mode {word:?}: say `{}` or `{}`",
            Self::WORK,
            Self::EXPLORE
        ))
    }

    /// Reads the column: a row this build cannot read never asked either.
    pub fn of(column: Option<&str>) -> Mode {
        match column {
            Some(word) => Self::parse(word).unwrap_or_default(),
            None => Mode::Work,
        }
    }
}

/// The workspace as this session last left it, and the probe that compares.
///
/// # Invariant
///
/// One session's snapshot, read and written by that session's checks alone, so a second
/// check waits for the first: two probes reading the same file at once would each report
/// the other's aftermath. A directory that is not a repository, or this machine's `git`
/// being unusable, blinds the watch for the rest of the session instead of paying a
/// failed spawn per job.
#[derive(Debug, Default)]
pub struct Watch {
    state: Mutex<State>,
}

/// One path as a look saw it: the status line, and a fingerprint of the contents.
///
/// # Invariant
///
/// A dirty path keeps printing the same line however often its file changes, so the line
/// alone cannot show a second edit. The fingerprint can, and neither half works without
/// the other.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Looked {
    line: String,
    fingerprint: String,
}

#[derive(Debug, Default)]
struct State {
    seen: Option<BTreeMap<String, Looked>>,
    blind: bool,
}

/// What one look at the workspace found.
#[derive(Debug, PartialEq, Eq)]
pub enum Look {
    /// Nothing to compare against: no repository, no `git`, or a probe that failed.
    Blind,
    /// The first look of this session: it is the shape later looks are compared to.
    First,
    /// The same workspace the last look saw.
    Same,
    /// The lines this session gained or lost, in the order `git` printed them.
    Changed(Vec<String>),
}

impl Watch {
    /// Compares `cwd` with what this watch last saw, then keeps this look.
    pub async fn look(&self, cwd: &Path) -> Look {
        if self.state.lock().map(|state| state.blind).unwrap_or(true) {
            return Look::Blind;
        }
        let Some(now) = probe(cwd).await else {
            if let Ok(mut state) = self.state.lock() {
                state.blind = true;
            }
            return Look::Blind;
        };
        let Ok(mut state) = self.state.lock() else {
            return Look::Blind;
        };
        let look = match state.seen.take() {
            None => Look::First,
            Some(before) => {
                let mut changed: Vec<String> = Vec::new();
                for (path, holds) in &now {
                    match before.get(path) {
                        Some(was) if was.fingerprint == holds.fingerprint => {}
                        _ => changed.push(holds.line.clone()),
                    }
                }
                for (path, was) in &before {
                    if !now.contains_key(path) {
                        changed.push(was.line.clone());
                    }
                }
                if changed.is_empty() {
                    Look::Same
                } else {
                    Look::Changed(changed)
                }
            }
        };
        state.seen = Some(now);
        look
    }
}

/// Every dirty path `git status --porcelain` reports, with its line and a content
/// fingerprint; `None` when unanswerable.
///
/// # Invariant
///
/// `GIT_OPTIONAL_LOCKS=0`: `git status` refreshes `.git/index` when it may, and a watcher
/// that writes what it watches finds its own aftermath.
///
/// The fingerprints are one `git hash-object --stdin-paths`, so a look costs two processes
/// and a file rewritten twice is two fingerprints, not one unchanged line.
async fn probe(cwd: &Path) -> Option<BTreeMap<String, Looked>> {
    let git = which::which("git").ok()?;
    let output = tokio::process::Command::new(&git)
        .arg("-C")
        .arg(cwd)
        .args(["status", "--porcelain"])
        .env("GIT_OPTIONAL_LOCKS", "0")
        .env("GIT_TERMINAL_PROMPT", "0")
        .stdin(std::process::Stdio::null())
        .output()
        .await
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&output.stdout);
    let mut rows: Vec<(String, String)> = Vec::new();
    for line in text.lines().filter(|line| !line.trim().is_empty()) {
        let Some(path) = status_path(line) else {
            continue;
        };
        rows.push((path, line.to_string()));
    }
    let present: Vec<&str> = rows
        .iter()
        .map(|(path, _)| path.as_str())
        .filter(|path| cwd.join(path).exists())
        .collect();
    let fingerprints = hash_paths(&git, cwd, &present).await;

    let mut seen = BTreeMap::new();
    for (path, line) in rows {
        let fingerprint = fingerprints
            .get(&path)
            .cloned()
            .unwrap_or_else(|| "absent".to_string());
        seen.insert(path, Looked { line, fingerprint });
    }
    Some(seen)
}

/// The path a `git status --porcelain` line names, with a rename's old name dropped.
fn status_path(line: &str) -> Option<String> {
    let rest = line.get(3..)?;
    let path = match rest.split_once(" -> ") {
        Some((_, new)) => new,
        None => rest,
    };
    Some(path.trim_matches('"').to_string())
}

/// One hash per existing path, keyed by path; a path `git` cannot hash stays `absent`.
async fn hash_paths(git: &Path, cwd: &Path, paths: &[&str]) -> BTreeMap<String, String> {
    let mut hashes = BTreeMap::new();
    if paths.is_empty() {
        return hashes;
    }
    use std::process::Stdio;
    use tokio::io::AsyncWriteExt;
    let Ok(mut child) = tokio::process::Command::new(git)
        .arg("-C")
        .arg(cwd)
        .args(["hash-object", "--stdin-paths"])
        .env("GIT_OPTIONAL_LOCKS", "0")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
    else {
        return hashes;
    };
    if let Some(mut stdin) = child.stdin.take() {
        let mut body = String::new();
        for path in paths {
            body.push_str(path);
            body.push('\n');
        }
        let _ = stdin.write_all(body.as_bytes()).await;
        drop(stdin);
    }
    let Ok(output) = child.wait_with_output().await else {
        return hashes;
    };
    let text = String::from_utf8_lossy(&output.stdout);
    for (path, hash) in paths.iter().zip(text.lines()) {
        hashes.insert((*path).to_string(), hash.to_string());
    }
    hashes
}

/// Takes the baseline a session's first look is measured against.
///
/// # Invariant
///
/// Without it the first change after the mode went on *is* the baseline and is never
/// reported: a look has nothing to compare a workspace it has never seen with.
pub fn prime(watch: Option<Arc<Watch>>, cwd: std::path::PathBuf) {
    let Some(watch) = watch else {
        return;
    };
    let Ok(handle) = tokio::runtime::Handle::try_current() else {
        return;
    };
    handle.spawn(async move {
        let _ = watch.look(&cwd).await;
    });
}

/// What to tell a session whose program changed the workspace it was only reading.
pub fn changed_notice(prompts: &crate::prompts::Prompts, tool: &str, changed: &[String]) -> String {
    let mut lines = String::new();
    for line in changed {
        lines.push_str("  ");
        lines.push_str(line);
        lines.push('\n');
    }
    prompts.render(
        crate::prompts::EXPLORE_CHANGED,
        &[("tool", tool), ("changed", lines.as_str())],
    )
}

/// What to tell a session when the mode changes under it.
pub fn mode_notice(prompts: &crate::prompts::Prompts, mode: Mode) -> String {
    match mode {
        Mode::Work => prompts.render(crate::prompts::WORK_NOTICE, &[]),
        Mode::Explore => prompts.render(crate::prompts::EXPLORE_NOTICE, &[]),
    }
}

/// The reminder for a refused write, so a caller learns the rule and the way around it
/// that is allowed.
pub fn refusal(prompts: &crate::prompts::Prompts, tool: &str) -> String {
    prompts.render(crate::prompts::EXPLORE_REFUSAL, &[("tool", tool)])
}

/// Looks at the workspace after a piece of work ended, and tells the session what changed.
///
/// # Invariant
///
/// Looks at the workspace a program's call left, and queues what changed.
///
/// # Invariant
///
/// The look is a process of its own, so the reminder costs the turn no waiting; what it
/// finds is news for the next step boundary, and `store::add_pending_task` orders it.
pub fn after_job(ctx: &ToolContext, tool: &str) {
    let Some(watch) = ctx.explore.clone() else {
        tracing::info!(tool, "explore: no watch on this call");
        return;
    };
    // A peer's workspace is not here: a look would read a same-named directory.
    if ctx.remote.is_some() {
        tracing::info!(
            tool,
            remote = ctx.remote.is_some(),
            "explore: skipped, the look is not owed"
        );
        return;
    }
    let cwd = ctx.cwd.clone();
    let store = ctx.store.clone();
    let session = ctx.session.clone();
    let tool = tool.to_string();
    tokio::spawn(async move {
        match watch.look(&cwd).await {
            Look::Changed(changed) => {
                let message = changed_notice(crate::prompts::shared(), &tool, &changed);
                if let Err(e) = tell(&store, &session, &message) {
                    tracing::warn!(session = %session, tool = %tool, error = %e,
                        "could not report a change to a workspace being read");
                }
            }
            other => {
                tracing::info!(tool, look = ?other, cwd = %cwd.display(), "explore: nothing to report")
            }
        }
    });
}

/// Writes the reminder as a notice, which owes an idle session no turn.
fn tell(store: &Store, session: &str, message: &str) -> anyhow::Result<i64> {
    store.add_pending_task(
        session,
        "inject",
        &json!({"session": session, "message": message, "notice": "explore"}),
    )
}

/// The watch a session reads with, when it may not write.
pub fn watch_of(mode: Mode) -> Option<std::sync::Arc<Watch>> {
    match mode {
        Mode::Work => None,
        Mode::Explore => Some(std::sync::Arc::new(Watch::default())),
    }
}
