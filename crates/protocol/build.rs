//! What build this is, for every program that links this crate.
//!
//! See `docs/notes/protocol-lib.md`.

use std::process::Command;

fn main() {
    let build = std::env::var("GONGGA_BUILD")
        .ok()
        .filter(|v| !v.is_empty())
        .or_else(from_git)
        .unwrap_or_else(|| "unknown".to_string());
    println!("cargo:rustc-env=GONGGA_BUILD={build}");
    println!("cargo:rerun-if-env-changed=GONGGA_BUILD");
    // Watch the ref, not HEAD: HEAD reads `ref: refs/heads/x` and never changes.
    if let Some(git) = git(&["rev-parse", "--git-dir"]) {
        let dir = std::path::PathBuf::from(git.trim());
        println!("cargo:rerun-if-changed={}", dir.join("HEAD").display());
        if let Some(head) = git_file(&dir) {
            println!("cargo:rerun-if-changed={}", head.display());
        }
        println!("cargo:rerun-if-changed={}", dir.join("index").display());
    }
}

/// The commit and whether the tree was dirty, or `None` where there is no git.
fn from_git() -> Option<String> {
    let sha = git(&["rev-parse", "--short=12", "HEAD"])?;
    let dirty = git(&["status", "--porcelain"]).is_some_and(|out| !out.trim().is_empty());
    Some(if dirty { format!("{sha}-dirty") } else { sha })
}

/// The file `HEAD` names, when it names a ref rather than a commit.
fn git_file(dir: &std::path::Path) -> Option<std::path::PathBuf> {
    let head = std::fs::read_to_string(dir.join("HEAD")).ok()?;
    let target = head.trim().strip_prefix("ref:")?.trim().to_string();
    Some(dir.join(target))
}

fn git(args: &[&str]) -> Option<String> {
    let out = Command::new("git").args(args).output().ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
}
