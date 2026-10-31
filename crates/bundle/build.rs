//! Embeds the programs when the `payload` feature asks for them.
//!
//! See `docs/notes/bundle-main.md`.

use std::path::{Path, PathBuf};
use std::process::Command;

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    // The payload is other packages' code; without this it stays stale until
    // this file is touched.
    if let Ok(root) = std::env::var("CARGO_MANIFEST_DIR") {
        let root = Path::new(&root).join("../..");
        println!("cargo:rerun-if-changed={}", root.join("crates").display());
        println!(
            "cargo:rerun-if-changed={}",
            root.join("Cargo.toml").display()
        );
        println!(
            "cargo:rerun-if-changed={}",
            root.join("Cargo.lock").display()
        );
    }
    // A commit touches nothing under `crates/`, but it changes what the embedded
    // programs report.
    if let Some(ref_file) = git_ref() {
        println!("cargo:rerun-if-changed={}", ref_file.display());
    }
    let out = PathBuf::from(std::env::var("OUT_DIR").expect("cargo always sets OUT_DIR"));
    let mut generated = String::new();
    if std::env::var_os("CARGO_FEATURE_PAYLOAD").is_some() {
        generated.push_str("pub const EMBEDDED: &[(&str, &[u8])] = &[\n");
        for (name, path) in built() {
            // From OUT_DIR: `include_bytes!` makes the file an input, and the
            // nested build rewrites its own output on every run.
            let copy = out.join(path.file_name().unwrap_or_default());
            std::fs::copy(&path, &copy).unwrap_or_else(|e| {
                panic!("copying {} to {}: {e}", path.display(), copy.display())
            });
            generated.push_str(&format!(
                "    ({name:?}, include_bytes!({:?})),\n",
                copy.display().to_string()
            ));
        }
        generated.push_str("];\n");
    } else {
        generated.push_str("pub const EMBEDDED: &[(&str, &[u8])] = &[];\n");
    }
    std::fs::write(out.join("embedded.rs"), generated).expect("writing the embedded table");
}

/// The ref a commit moves: `.git/HEAD` reads `ref: refs/heads/x` and never changes.
fn git_ref() -> Option<PathBuf> {
    let out = Command::new("git")
        .args(["rev-parse", "--git-dir"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let dir = PathBuf::from(String::from_utf8_lossy(&out.stdout).trim());
    let head = std::fs::read_to_string(dir.join("HEAD")).ok()?;
    let target = head.trim().strip_prefix("ref:")?.trim().to_string();
    Some(dir.join(target))
}

/// The programs, built here rather than expected to be lying around.
///
/// # Invariant
///
/// A nested `cargo` gets its own target directory: the parent holds a lock on
/// the one it is using, and a child that waited for it would deadlock the build
/// that started it. The profile is the parent's, so an embedded program is the
/// same kind of build as the head that carries it.
fn built() -> Vec<(&'static str, PathBuf)> {
    // `PROFILE` is a directory name; `--profile` wants the profile name, and
    // cargo reserves `debug` for the `dev` profile.
    let profile = match std::env::var("PROFILE").as_deref() {
        Ok("release") => "release",
        _ => "dev",
    };
    let target = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").expect("set by cargo"))
        .join("../../target/bundle-payload");
    let mut cmd = Command::new(std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_string()));
    cmd.args(["build", "--profile", profile])
        .args([
            "-p",
            "gongga-cli",
            "-p",
            "gongga-server",
            "-p",
            "gongga-tui",
        ])
        .current_dir(Path::new(&std::env::var("CARGO_MANIFEST_DIR").unwrap()).join("../.."))
        .env("CARGO_TARGET_DIR", &target)
        // The payload is built for the moment this file is compiled: inheriting
        // the feature would make the nested build of the programs a bundle too.
        .env_remove("CARGO_FEATURE_PAYLOAD");
    let output = cmd
        .output()
        .expect("running cargo for the embedded payload");
    assert!(
        output.status.success(),
        "building the embedded programs failed:\n{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let suffix = if cfg!(windows) { ".exe" } else { "" };
    ["gongga-cli", "gongga-server", "gongga-tui"]
        .into_iter()
        .map(|name| {
            (
                name,
                target
                    .join(if profile == "dev" { "debug" } else { profile })
                    .join(format!("{name}{suffix}")),
            )
        })
        .collect()
}
