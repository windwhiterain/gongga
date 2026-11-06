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
use std::sync::Mutex;

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

#[derive(Debug, Default)]
struct State {
    seen: Option<BTreeMap<String, ()>>,
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
        let Some(lines) = probe(cwd).await else {
            if let Ok(mut state) = self.state.lock() {
                state.blind = true;
            }
            return Look::Blind;
        };
        let mut now = BTreeMap::new();
        for line in lines {
            now.insert(line, ());
        }
        let Ok(mut state) = self.state.lock() else {
            return Look::Blind;
        };
        let look = match state.seen.take() {
            None => Look::First,
            Some(before) => {
                let changed: Vec<String> = now
                    .keys()
                    .filter(|line| !before.contains_key(*line))
                    .chain(before.keys().filter(|line| !now.contains_key(*line)))
                    .cloned()
                    .collect();
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

/// The lines `git status --porcelain` prints for `cwd`; `None` when unanswerable.
///
/// # Invariant
///
/// `GIT_OPTIONAL_LOCKS=0`, because the probe must not write: `git status` refreshes
/// `.git/index` when it may, and a watcher that writes the workspace it watches finds
/// its own aftermath on the next look.
async fn probe(cwd: &Path) -> Option<Vec<String>> {
    let git = which::which("git").ok()?;
    let output = tokio::process::Command::new(git)
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
    Some(
        text.lines()
            .filter(|line| !line.trim().is_empty())
            .map(|line| line.to_string())
            .collect(),
    )
}

/// Whether this tool runs a program of the operator's choosing, so only its effects can
/// say what it did.
///
/// # Invariant
///
/// A tool that is named here is checked; a tool that writes through its own arguments
/// (`write_file`, `replace`) is refused instead, since its identity is the write.
fn is_program(name: &str) -> bool {
    matches!(name, "bash" | "exec" | "python" | "skill") || name.starts_with("remote_")
}

/// What to tell a session whose program changed the workspace it was only reading.
pub fn changed_notice(tool: &str, changed: &[String]) -> String {
    let mut lines = String::new();
    for line in changed {
        lines.push_str("  ");
        lines.push_str(line);
        lines.push('\n');
    }
    format!(
        "This session explores: the `{tool}` call that just ended changed the workspace.\n\n\
         {lines}\n\
         Nothing was undone. Read those paths again before saying what they hold, and if a \
         change was not yours to make, put it back (`git checkout -- <path>`, or `git stash` \
         what you did not write) and say that you did."
    )
}

/// What to tell a session when the mode changes under it.
pub fn mode_notice(mode: Mode) -> String {
    match mode {
        Mode::Work => "This session works: it may write, and a program's changes are not \
                       reported."
            .to_string(),
        Mode::Explore => "This session explores: `write_file` and `replace` are refused, and \
                          a program that changes the workspace is reported at the end of the \
                          job that changed it. This is not a jail -- a shell can still write -- \
                          so read before you change, and say what you changed."
            .to_string(),
    }
}

/// The reminder for a refused write, so a caller learns the rule and the way around it
/// that is allowed.
pub fn refusal(tool: &str) -> String {
    format!(
        "This session explores, so `{tool}` is refused: it changes the workspace, and the \
         command that made this session asked for a read. Read with `read_file`, and say what \
         would have to change rather than changing it."
    )
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
        return;
    };
    // A peer's workspace is not here: a look would read a same-named directory.
    if ctx.remote.is_some() || !is_program(tool) {
        return;
    }
    let cwd = ctx.cwd.clone();
    let store = ctx.store.clone();
    let session = ctx.session.clone();
    let tool = tool.to_string();
    tokio::spawn(async move {
        match watch.look(&cwd).await {
            Look::Changed(changed) => {
                let message = changed_notice(&tool, &changed);
                if let Err(e) = tell(&store, &session, &message) {
                    tracing::warn!(session = %session, tool = %tool, error = %e,
                        "could not report a change to a workspace being read");
                }
            }
            Look::Blind | Look::First | Look::Same => {}
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
