//! SPEC's `Workspace.worktree`: the branch and directory a session is born in.
//!
//! See `docs/notes/server-worktree.md`.

use std::path::{Path, PathBuf};

use crate::space::{Run, Space};

/// Where a repository's worktrees are made: beside its first worktree, as this
/// repository keeps its own.
const HOME: &str = ".worktrees";

/// The worktree a caller's `workspace.worktree` asks for.
///
/// # Invariant
///
/// SPEC spells none as `none`, and an empty word is the same silence: neither makes a
/// branch, so a config that says `none` reads as one that says nothing.
pub fn requested(stated: Option<&str>) -> Option<&str> {
    stated
        .map(str::trim)
        .filter(|name| !name.is_empty() && *name != protocol::NONE_VALUE)
}

/// What removing a session's worktree left behind.
#[derive(Debug, Default)]
pub struct Removed {
    /// The worktree directory taken away, when it was still there to take.
    pub path: Option<PathBuf>,
    /// Whether a branch of that name went with it.
    pub branch: bool,
}

/// One worktree as `git worktree list --porcelain` states it.
struct Listed {
    path: PathBuf,
    /// The branch checked out there, as git spells references: `refs/heads/<name>`.
    branch: Option<String>,
    bare: bool,
}

/// The repository `start` sits inside, as the machine that owns it answers.
struct Repository {
    /// Its first worktree: where git says its worktrees begin, and where a new one is
    /// made beside.
    main: PathBuf,
    /// Every worktree it has, in git's own order.
    worktrees: Vec<Listed>,
}

impl Repository {
    /// The repository `start` sits inside, or nothing when it sits inside none.
    ///
    /// # Invariant
    ///
    /// Git is asked on the machine that owns the directory, standing in that directory:
    /// the answer is that machine's, never this one's, and a refusal is read as "no
    /// repository here" rather than as a failure to ask.
    async fn of(space: &Space, start: &Path) -> Result<Option<Self>, String> {
        let run = git(space, start, &["worktree", "list", "--porcelain"]).await?;
        if !run.succeeded() {
            return Ok(None);
        }
        let worktrees = parse(&run.stdout_text());
        let Some(main) = worktrees
            .iter()
            .find(|entry| !entry.bare)
            .map(|entry| entry.path.clone())
        else {
            return Ok(None);
        };
        Ok(Some(Self { main, worktrees }))
    }

    /// The worktree `name` is out at, when one of them has it checked out.
    fn holder(&self, name: &str) -> Option<&Listed> {
        self.worktrees
            .iter()
            .find(|entry| entry.branch.as_deref().and_then(branch_of) == Some(name))
    }
}

/// Makes a worktree named `name` in the repository `start` sits in, and answers its root.
///
/// # Invariant
///
/// The branch and the directory are made together or not at all, so a refusal leaves
/// nothing for a later session of the same name to collide with: the branch is drawn
/// from the repository's first worktree, never from wherever `start` happens to be.
pub async fn create(space: &Space, start: &Path, name: &str) -> Result<PathBuf, String> {
    let repository = Repository::of(space, start).await?.ok_or_else(|| {
        format!(
            "a worktree named {name:?} needs a repository, and {} sits inside none",
            start.display()
        )
    })?;
    // Everything after discovery runs in the repository's first worktree.
    let root = &repository.main;
    check_name(space, root, name).await?;
    // One refusal per collision, in the words of the thing that is already there.
    if let Some(entry) = repository.holder(name) {
        return Err(format!(
            "a branch named {name:?} is already out at {}",
            entry.path.display()
        ));
    }
    if holds_branch(space, root, name).await? {
        return Err(format!(
            "a branch named {name:?} already exists in {}",
            root.display()
        ));
    }
    let target = repository.main.join(HOME).join(name);
    let text = target.to_string_lossy().into_owned();
    let run = git(space, root, &["worktree", "add", &text, "-b", name]).await?;
    if !run.succeeded() {
        return Err(format!(
            "git could not make the worktree {name:?}: {}",
            first_line(&run)
        ));
    }
    Ok(target)
}

/// Removes the worktree named `name` and its branch, from the repository `cwd` sits in.
///
/// # Invariant
///
/// Forced: the caller said the session is gone, so uncommitted work goes with it. A git
/// refusal -- a file held open, the branch out somewhere else -- is an error, never a
/// warning: what is left would be a branch no session removes.
///
/// A `cwd` in no repository is a worktree already gone, and the answer says so.
pub async fn remove(space: &Space, name: &str, cwd: &Path) -> Result<Removed, String> {
    let Some(repository) = Repository::of(space, cwd).await? else {
        return Ok(Removed::default());
    };
    let root = &repository.main;
    let mut removed = Removed::default();
    if let Some(path) = repository.holder(name).map(|entry| entry.path.clone()) {
        let text = path.to_string_lossy().into_owned();
        // Never from inside it: a program standing there holds the directory open.
        let run = git(space, root, &["worktree", "remove", "--force", &text]).await?;
        if run.succeeded() {
            removed.path = Some(path);
        } else {
            // Gone without git, perhaps: prune clears what it left, and a registration
            // still listed is one this refuses to drop.
            let pruned = git(space, root, &["worktree", "prune"]).await?;
            let left = match Repository::of(space, root).await? {
                Some(after) => after.holder(name).is_some(),
                None => false,
            };
            if left || !pruned.succeeded() {
                return Err(format!(
                    "the worktree at {} could not be removed: {}",
                    path.display(),
                    first_line(&run)
                ));
            }
        }
    }
    if holds_branch(space, root, name).await? {
        let run = git(space, root, &["branch", "-D", name]).await?;
        if !run.succeeded() {
            return Err(format!(
                "the branch {name:?} could not be deleted: {}",
                first_line(&run)
            ));
        }
        removed.branch = true;
    }
    Ok(removed)
}

/// Every worktree in git's own porcelain, one entry per blank-line-separated block.
fn parse(text: &str) -> Vec<Listed> {
    let mut list = Vec::new();
    for block in text.replace("\r\n", "\n").split("\n\n") {
        let mut path = None;
        let mut branch = None;
        let mut bare = false;
        for line in block.lines() {
            if let Some(rest) = line.strip_prefix("worktree ") {
                path = Some(PathBuf::from(rest.trim()));
            } else if let Some(rest) = line.strip_prefix("branch ") {
                branch = Some(rest.trim().to_string());
            } else if line == "bare" {
                bare = true;
            }
        }
        if let Some(path) = path {
            list.push(Listed { path, branch, bare });
        }
    }
    list
}

/// The branch a reference names, or nothing for a detached worktree.
fn branch_of(reference: &str) -> Option<&str> {
    reference.strip_prefix("refs/heads/")
}

/// Whether the repository already has a branch named `name`.
async fn holds_branch(space: &Space, where_: &Path, name: &str) -> Result<bool, String> {
    let reference = format!("refs/heads/{name}");
    let run = git(space, where_, &["rev-parse", "--verify", "-q", &reference]).await?;
    Ok(run.succeeded())
}

/// Refuses a name git would not take as a branch, before anything is made of it.
///
/// # Invariant
///
/// Git's own rule is the whole rule: the name is a branch name first, and the directory
/// it becomes follows from it, so a name git refuses is refused here.
async fn check_name(space: &Space, where_: &Path, name: &str) -> Result<(), String> {
    let run = git(space, where_, &["check-ref-format", "--branch", name]).await?;
    if run.succeeded() {
        return Ok(());
    }
    Err(format!(
        "{name:?} is not a branch name: {}",
        first_line(&run)
    ))
}

/// `git`, on the machine that owns `cwd`, standing in it.
async fn git(space: &Space, cwd: &Path, args: &[&str]) -> Result<Run, String> {
    let args: Vec<String> = args.iter().map(|arg| arg.to_string()).collect();
    space.run(cwd, "git", &args, None).await
}

/// Git's own first complaint, or that it made none.
fn first_line(run: &Run) -> String {
    let said = String::from_utf8_lossy(&run.stderr);
    said.lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or("git said nothing")
        .to_string()
}
