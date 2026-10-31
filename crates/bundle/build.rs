//! Embeds the programs when the `payload` feature asks for them.
//!
//! See `docs/notes/bundle-main.md`.

use std::path::{Path, PathBuf};
use std::process::Command;

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    let out = PathBuf::from(std::env::var("OUT_DIR").expect("cargo always sets OUT_DIR"));
    let mut generated = String::new();
    if std::env::var_os("CARGO_FEATURE_PAYLOAD").is_some() {
        let payloads = built();
        generated.push_str("pub const EMBEDDED: &[(&str, &[u8])] = &[\n");
        for (name, path) in &payloads {
            generated.push_str(&format!(
                "    ({name:?}, include_bytes!({:?})),\n",
                path.display().to_string()
            ));
        }
        generated.push_str("];\n");
        for (_, path) in &payloads {
            println!("cargo:rerun-if-changed={}", path.display());
        }
    } else {
        generated.push_str("pub const EMBEDDED: &[(&str, &[u8])] = &[];\n");
    }
    std::fs::write(out.join("embedded.rs"), generated).expect("writing the embedded table");
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
    // `PROFILE` is the directory name; `--profile` wants the profile's name, and
    // the development one is `dev` -- `debug` is reserved and cargo refuses it.
    let profile = match std::env::var("PROFILE").as_deref() {
        Ok("release") => "release",
        _ => "dev",
    };
    let target = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").expect("set by cargo"))
        .join("../../target/bundle-payload");
    let mut cmd = Command::new(std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_string()));
    cmd.args(["build", "--profile", profile])
        .args(["-p", "gongga-cli", "-p", "gongga-server"])
        .current_dir(Path::new(&std::env::var("CARGO_MANIFEST_DIR").unwrap()).join("../.."))
        .env("CARGO_TARGET_DIR", &target)
        .env_remove("CARGO_FEATURE_PAYLOAD");
    let output = cmd.output().expect("running cargo for the embedded payload");
    assert!(
        output.status.success(),
        "building the embedded programs failed:\n{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let suffix = if cfg!(windows) { ".exe" } else { "" };
    ["gongga-cli", "gongga-server"]
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
