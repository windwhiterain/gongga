//! SPEC's `Workspace.worktree`: the branch and directory a session is born in.
//!
//! See `docs/notes/server-worktree.md`.

use std::path::{Path, PathBuf};

/// Where a repository's worktrees are made: beside its main worktree, as this
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

/// Makes a worktree named `name` in the repository `start` sits in, and answers its
/// root.
///
/// # Invariant
///
/// The branch and the directory are made together or not at all, so a refusal leaves
/// nothing for a later session of the same name to collide with: the branch is drawn
/// from the repository's first worktree, never from wherever `start` happens to be.
pub fn create(start: &Path, name: &str) -> Result<PathBuf, String> {
    let common = repository(start)?.ok_or_else(|| {
        format!(
            "a worktree named {name:?} needs a repository, and {} sits inside none",
            start.display()
        )
    })?;
    let list = listed(&common)?;
    let main = list
        .iter()
        .find(|entry| !entry.bare)
        .map(|entry| entry.path.clone())
        .ok_or_else(|| {
            format!(
                "the repository at {} has no main worktree to make another beside",
                common.display()
            )
        })?;
    check_name(&common, name)?;
    // One refusal per collision, in the words of the thing that is already there.
    if let Some(entry) = list
        .iter()
        .find(|entry| entry.branch.as_deref().and_then(branch_of) == Some(name))
    {
        return Err(format!(
            "a branch named {name:?} is already out at {}",
            entry.path.display()
        ));
    }
    if holds_branch(&common, name)? {
        return Err(format!(
            "a branch named {name:?} already exists in {}",
            main.display()
        ));
    }
    let target = main.join(HOME).join(name);
    if target.exists() {
        return Err(format!(
            "something is already at {}, where the worktree {name:?} would be made",
            target.display()
        ));
    }
    let text = target.to_string_lossy().into_owned();
    let out = git(
        &common,
        Some(&main),
        &["worktree", "add", &text, "-b", name],
    )?;
    if !out.status.success() {
        // Git may have made the directory before refusing; an empty one stops the next try.
        let _ = std::fs::remove_dir(&target);
        return Err(format!(
            "git could not make the worktree {name:?}: {}",
            said(&out)
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
pub fn remove(name: &str, cwd: &Path) -> Result<Removed, String> {
    let Some(common) = repository(cwd)? else {
        return Ok(Removed::default());
    };
    let mut removed = Removed::default();
    if let Some(entry) = listed(&common)?
        .into_iter()
        .find(|entry| entry.branch.as_deref().and_then(branch_of) == Some(name))
    {
        let text = entry.path.to_string_lossy().into_owned();
        let out = git(&common, None, &["worktree", "remove", "--force", &text])?;
        if out.status.success() {
            removed.path = Some(entry.path);
        } else if entry.path.exists() {
            return Err(format!(
                "the worktree at {} could not be removed: {}",
                entry.path.display(),
                said(&out)
            ));
        } else {
            // The directory went without git: the registration it left is what prune takes.
            let out = git(&common, None, &["worktree", "prune"])?;
            if !out.status.success() {
                return Err(format!(
                    "the worktree registration could not be pruned: {}",
                    said(&out)
                ));
            }
        }
    }
    if holds_branch(&common, name)? {
        let out = git(&common, None, &["branch", "-D", name])?;
        if !out.status.success() {
            return Err(format!(
                "the branch {name:?} could not be deleted: {}",
                said(&out)
            ));
        }
        removed.branch = true;
    }
    Ok(removed)
}

/// One worktree as `git worktree list --porcelain` states it.
struct Listed {
    path: PathBuf,
    /// The branch checked out there, as git spells references: `refs/heads/<name>`.
    branch: Option<String>,
    bare: bool,
}

/// The directory a checkout's worktrees are registered in, when `start` sits in one.
///
/// # Invariant
///
/// The checkout's own `.git` is read, and nothing else is searched for: a directory
/// that names no git directory is not a repository, whatever sits above or below it.
fn repository(start: &Path) -> Result<Option<PathBuf>, String> {
    let mut here = std::path::absolute(start).ok();
    while let Some(dir) = here {
        if let Some(git) = crate::fork::git_dir(&dir) {
            return crate::fork::common_dir(&git)
                .map(Some)
                .ok_or_else(|| format!("git could not read the repository at {}", git.display()));
        }
        here = dir.parent().map(Path::to_path_buf);
    }
    Ok(None)
}

/// Every worktree of the repository whose git directory is `common`, in git's order.
fn listed(common: &Path) -> Result<Vec<Listed>, String> {
    let out = git(common, None, &["worktree", "list", "--porcelain"])?;
    if !out.status.success() {
        return Err(format!("git could not list the worktrees: {}", said(&out)));
    }
    let text = String::from_utf8_lossy(&out.stdout).replace("\r\n", "\n");
    let mut list = Vec::new();
    for block in text.split("\n\n") {
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
    Ok(list)
}

/// The branch a reference names, or nothing for a detached worktree.
fn branch_of(reference: &str) -> Option<&str> {
    reference.strip_prefix("refs/heads/")
}

/// Whether the repository already has a branch named `name`.
fn holds_branch(common: &Path, name: &str) -> Result<bool, String> {
    let reference = format!("refs/heads/{name}");
    let out = git(common, None, &["rev-parse", "--verify", "-q", &reference])?;
    Ok(out.status.success())
}

/// Refuses a name git would not take as a branch, before anything is made of it.
///
/// # Invariant
///
/// Git's own rule is the whole rule: the name is a branch name first, and the
/// directory it becomes follows from it, so a name git refuses is refused here.
fn check_name(common: &Path, name: &str) -> Result<(), String> {
    let out = git(common, None, &["check-ref-format", "--branch", name])?;
    if out.status.success() {
        return Ok(());
    }
    Err(format!("{name:?} is not a branch name: {}", said(&out)))
}

/// `git` in one repository, with the directory named so nothing is searched for.
fn git(
    common: &Path,
    work_tree: Option<&Path>,
    args: &[&str],
) -> Result<std::process::Output, String> {
    crate::fork::try_git_at(common, work_tree, args)
        .map_err(|e| format!("could not run `git {}`: {e}", args.join(" ")))
}

/// Git's own first complaint, or that it made none.
fn said(out: &std::process::Output) -> String {
    String::from_utf8_lossy(&out.stderr)
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or("git said nothing")
        .to_string()
}
