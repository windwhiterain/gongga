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
/// check waits for the first. A workspace this watch cannot compare is described to the
/// session once, never kept as a silence. See `docs/notes/server-explore.md`.
#[derive(Debug, Default)]
pub struct Watch {
    state: Mutex<State>,
}

#[derive(Debug, Default)]
struct State {
    /// The lines of the last look that could compare.
    seen: Option<BTreeMap<String, ()>>,
    /// The shape this watch has already described, so it is described once.
    said: Option<Shape>,
}

impl State {
    /// Whether this shape is news: true the first time it is found, false after.
    fn say(&mut self, shape: Shape) -> bool {
        self.said.replace(shape) != Some(shape)
    }
}

/// The shapes of a look that is not a comparison.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Shape {
    /// No repository covers the directory.
    Unversioned,
    /// The probe itself could not answer.
    Unreadable,
}

/// What a session has to hear about the workspace it is reading.
#[derive(Debug, PartialEq, Eq)]
pub enum News {
    /// This machine cannot read the directory, and this is what it said.
    Blind(String),
    /// No repository covers the directory: nothing here can be compared.
    Unversioned,
    /// The first look found a repository that was already dirty.
    AlreadyDirty(Vec<String>),
    /// The lines the workspace gained or lost since the last look.
    Changed(Vec<String>),
}

impl Watch {
    /// Compares `cwd` with what this watch last saw, and keeps this look.
    ///
    /// # Invariant
    ///
    /// News, not a state: a directory with no repository, or one this machine cannot
    /// read, is described when the watch first meets it and not again, so a watch that
    /// cannot see says so and a job that changed nothing says nothing.
    pub async fn look(&self, cwd: &Path) -> Option<News> {
        let found = probe(cwd).await;
        let Ok(mut state) = self.state.lock() else {
            return None;
        };
        match found {
            Found::Unreadable(why) => {
                state.seen = None;
                state.say(Shape::Unreadable).then_some(News::Blind(why))
            }
            Found::Unversioned => {
                state.seen = None;
                state.say(Shape::Unversioned).then_some(News::Unversioned)
            }
            Found::Lines(lines) => {
                state.said = None;
                let now: BTreeMap<String, ()> =
                    lines.iter().map(|line| (line.clone(), ())).collect();
                match state.seen.replace(now.clone()) {
                    None => (!lines.is_empty()).then_some(News::AlreadyDirty(lines)),
                    Some(before) => {
                        let changed: Vec<String> = now
                            .keys()
                            .filter(|line| !before.contains_key(*line))
                            .chain(before.keys().filter(|line| !now.contains_key(*line)))
                            .cloned()
                            .collect();
                        (!changed.is_empty()).then_some(News::Changed(changed))
                    }
                }
            }
        }
    }

    /// Records that this machine cannot read the workspace at all, and says so once.
    pub fn unreadable(&self, why: String) -> Option<News> {
        let Ok(mut state) = self.state.lock() else {
            return None;
        };
        state.seen = None;
        state.say(Shape::Unreadable).then_some(News::Blind(why))
    }
}

/// What one `git status --porcelain` answered about `cwd`.
enum Found {
    /// The lines `git status` printed.
    Lines(Vec<String>),
    /// `git` answered that no repository covers the directory.
    Unversioned,
    /// The probe could not answer, and this is what it said.
    Unreadable(String),
}

/// The lines `git status --porcelain` prints for `cwd`, or why there are none.
///
/// # Invariant
///
/// `GIT_OPTIONAL_LOCKS=0`, because the probe must not write: `git status` refreshes
/// `.git/index` when it may, and a watcher that writes the workspace it watches finds
/// its own aftermath on the next look.
async fn probe(cwd: &Path) -> Found {
    let Ok(git) = which::which("git") else {
        return Found::Unreadable("this machine has no `git` on its PATH".to_string());
    };
    let output = tokio::process::Command::new(git)
        .arg("-C")
        .arg(cwd)
        .args(["status", "--porcelain"])
        .env("GIT_OPTIONAL_LOCKS", "0")
        .env("GIT_TERMINAL_PROMPT", "0")
        .stdin(std::process::Stdio::null())
        .output()
        .await;
    let Ok(output) = output else {
        return Found::Unreadable(format!(
            "`git` could not be started for `{}`",
            cwd.display()
        ));
    };
    if !output.status.success() {
        // `git`'s own word for a directory it found no repository above.
        let said = String::from_utf8_lossy(&output.stderr).trim().to_string();
        if said.contains("not a git repository") {
            return Found::Unversioned;
        }
        return Found::Unreadable(match said.is_empty() {
            true => format!("`git -C {} status` failed", cwd.display()),
            false => said,
        });
    }
    let text = String::from_utf8_lossy(&output.stdout);
    Found::Lines(
        text.lines()
            .filter(|line| !line.trim().is_empty())
            .map(|line| line.to_string())
            .collect(),
    )
}

/// Who hears what a look finds: the session's store, and the session's name.
#[derive(Clone)]
pub struct Ear {
    store: Store,
    session: String,
}

impl Ear {
    /// The notice queue a machine's looks about this session are queued in.
    pub fn of(store: &Store, session: &str) -> Self {
        Self {
            store: store.clone(),
            session: session.to_string(),
        }
    }

    /// Queues what a look found as a notice, which owes an idle session no turn.
    fn report(&self, message: String) {
        if let Err(e) = tell(&self.store, &self.session, &message) {
            tracing::warn!(session = %self.session, error = %e,
                "could not tell a session about the workspace it reads");
        }
    }
}

/// Why a peer's directory cannot be read from here.
fn cannot_read(remote: &str, cwd: &Path) -> String {
    format!(
        "this session's shells run on remote `{remote}`, so `{}` is not on this machine",
        cwd.display()
    )
}

/// Takes the baseline a session's first look is measured against, and says what it found.
///
/// # Invariant
///
/// Without it the first change after the mode went on *is* the baseline and is never
/// reported: a look has nothing to compare a workspace it has never seen with. A first
/// look that is not a clean repository -- none at all, a dirty one, or a directory this
/// machine cannot read -- is said, because silence would read as `Same`.
pub fn prime(
    ear: &Ear,
    watch: Option<Arc<Watch>>,
    cwd: std::path::PathBuf,
    remote: Option<String>,
) {
    let Some(watch) = watch else {
        return;
    };
    let Ok(handle) = tokio::runtime::Handle::try_current() else {
        return;
    };
    let ear = ear.clone();
    handle.spawn(async move {
        let news = match remote {
            Some(remote) => watch.unreadable(cannot_read(&remote, &cwd)),
            None => watch.look(&cwd).await,
        };
        let Some(news) = news else {
            return;
        };
        // A change is the job's to report; the primer is not that job.
        if let Some(message) = notice(&cwd, &news) {
            ear.report(message);
        }
    });
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

/// The words for a workspace this watch cannot compare; `None` for a change, which is a
/// job's news.
pub fn notice(cwd: &Path, news: &News) -> Option<String> {
    let here = cwd.display();
    let words = match news {
        News::Unversioned => format!(
            "This session explores, but `{here}` is in no git repository, so this machine \
             cannot tell a change from the workspace it found. Run `git init` in that \
             directory, then `git add -A` and commit it, before you read: an untracked tree \
             is not a baseline either, and nothing you do there is reported until it is one."
        ),
        News::AlreadyDirty(lines) => {
            let mut listed = String::new();
            for line in lines {
                listed.push_str("  ");
                listed.push_str(line);
                listed.push('\n');
            }
            format!(
                "This session explores, and `{here}` is a git repository that was already \
                 dirty at this session's first look:\n\n{listed}\n\
                 Make a worktree of your own and read there -- `git worktree add <path> HEAD`, \
                 with a path outside that tree -- so every later change belongs to this \
                 session and to nobody else. If those lines are your own uncommitted work, \
                 commit them instead, so the watch has a baseline."
            )
        }
        News::Blind(why) => format!(
            "This session explores, but this machine cannot read its workspace: {why}. \
             Nothing a program does there can be reported as a change while that holds. \
             The directory it could not read is `{here}`."
        ),
        News::Changed(_) => return None,
    };
    Some(words)
}

/// Looks at the workspace after a piece of work ended, and tells the session what changed.
///
/// # Invariant
///
/// The look is a process of its own, so the reminder costs the turn no waiting; what it
/// finds is news for the next step boundary, and `store::add_pending_task` orders it.
pub fn after_job(ctx: &ToolContext, tool: &str) {
    let Some(watch) = ctx.explore.clone() else {
        return;
    };
    if !is_program(tool) {
        return;
    }
    let ear = Ear::of(&ctx.store, &ctx.session);
    let cwd = ctx.cwd.clone();
    let remote = ctx.remote.clone();
    let tool = tool.to_string();
    tokio::spawn(async move {
        let news = match remote {
            Some(remote) => watch.unreadable(cannot_read(&remote, &cwd)),
            None => watch.look(&cwd).await,
        };
        let Some(news) = news else {
            return;
        };
        let message = match &news {
            News::Changed(changed) => changed_notice(&tool, changed),
            other => match notice(&cwd, other) {
                Some(message) => message,
                None => return,
            },
        };
        ear.report(message);
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
