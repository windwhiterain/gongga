//! The head of a distribution: one file that carries the programs.
//!
//! See `docs/notes/bundle-main.md`.

mod payload_impl;

use std::path::{Path, PathBuf};
use std::process::ExitCode;

/// The program to run when the name this file was started under says nothing.
const DEFAULT: &str = "gongga-cli";

fn main() -> ExitCode {
    match run() {
        Ok(code) => ExitCode::from(code),
        Err(e) => {
            eprintln!("gongga: {e}");
            ExitCode::from(1)
        }
    }
}

fn run() -> Result<u8, String> {
    let (wanted, skip) = wanted_program();
    let carried: Vec<&str> = payload::EMBEDDED.iter().map(|(name, _)| *name).collect();
    if carried.is_empty() {
        return Err(format!(
            "this file carries no programs: it was built with `--no-default-features`, \
             so there is nothing to place. Install it without that flag \
             (`cargo install --git <repo> gongga-bundle`)"
        ));
    }
    if !carried.contains(&wanted.as_str()) {
        return Err(format!(
            "this file does not carry {wanted}; it carries {carried:?}"
        ));
    }
    // Beside this build: a server may be running from `bin`, which no platform
    // can replace. See `bundle-main.md`.
    let stamp = payload::stamp(payload::EMBEDDED);
    let dir = home().join("payload").join(&stamp);
    // Every program, not only the one asked for: `serve` looks for the server
    // beside itself.
    for (name, bytes) in payload::EMBEDDED {
        payload::place(&dir, name, bytes)
            .map_err(|e| format!("placing {name} in {}: {e}", dir.display()))?;
    }
    // A build this replaces is asked to stop, then removed -- a server through
    // its own `cancel`, so its turn survives.
    payload::retire(&home().join("payload"), &stamp, &dir);
    take_over(&dir.join(payload::file_name(&wanted)), skip)
}

/// The programs, embedded at build time by the `payload` feature.
mod payload {
    include!(concat!(env!("OUT_DIR"), "/embedded.rs"));

    pub use crate::payload_impl::{file_name, place, retire, stamp};
}

/// Which program to run, and how many arguments that choice consumed.
///
/// # Invariant
///
/// The file name wins, because a link cannot carry an argument: a copy named
/// `gongga-server` is the server. Failing that, a leading word names the program
/// and is taken off before that program sees it.
///
/// # Invariant
///
/// The word wins over the server's own command list, which this file cannot see
/// before it has a server to ask. `cli`, `tui` and `server` are therefore reserved
/// here: a script that registered a command by one of those names would be
/// unreachable through this file, and reachable through `gongga-cli`.
fn wanted_program() -> (String, usize) {
    if let Some(name) = by_file_name() {
        return (name, 0);
    }
    if let Some(word) = std::env::args_os()
        .nth(1)
        .and_then(|arg| arg.into_string().ok())
    {
        for (spoken, program) in WORDS {
            if word == *spoken {
                return ((*program).to_string(), 1);
            }
        }
    }
    (DEFAULT.to_string(), 0)
}

/// The words this file answers to. `serve` is absent: it is the layer's own.
const WORDS: [(&str, &str); 4] = [
    ("cli", "gongga-cli"),
    ("tui", "gongga-tui"),
    ("window", "gongga-tui"),
    ("server", "gongga-server"),
];

/// The program this file is named after, when it is named after one of them.
fn by_file_name() -> Option<String> {
    std::env::args_os()
        .next()
        .map(PathBuf::from)
        .and_then(|path| {
            path.file_stem()
                .map(|stem| stem.to_string_lossy().into_owned())
        })
        .map(|stem| stem.to_ascii_lowercase())
        .filter(|stem| matches!(stem.as_str(), "gongga-server" | "gongga-tui"))
}

/// `--port <p>` rewritten to `--bind 127.0.0.1:<p>`, when no `--bind` was given.
fn bind_from_port(args: Vec<std::ffi::OsString>) -> Vec<std::ffi::OsString> {
    let as_strings: Vec<String> = args
        .iter()
        .map(|arg| arg.to_string_lossy().into_owned())
        .collect();
    if as_strings
        .iter()
        .any(|arg| arg == "--bind" || arg.starts_with("--bind="))
    {
        return args;
    }
    let Some(at) = as_strings.iter().position(|arg| arg == "--port") else {
        return args;
    };
    let Some(port) = as_strings.get(at + 1) else {
        return args;
    };
    let mut rewritten: Vec<std::ffi::OsString> = as_strings[..at]
        .iter()
        .map(std::ffi::OsString::from)
        .collect();
    rewritten.push("--bind".into());
    rewritten.push(format!("127.0.0.1:{port}").into());
    rewritten.extend(as_strings[at + 2..].iter().map(std::ffi::OsString::from));
    rewritten
}

/// The home: `GONGGA_HOME`, else `~/.gongga`. Same order as the programs carry.
fn home() -> PathBuf {
    if let Some(named) = std::env::var_os("GONGGA_HOME").filter(|v| !v.is_empty()) {
        return PathBuf::from(named);
    }
    let user = std::env::var_os("USERPROFILE").or_else(|| std::env::var_os("HOME"));
    user.map(|home| PathBuf::from(home).join(".gongga"))
        .unwrap_or_else(|| PathBuf::from(".gongga"))
}

/// Runs `program` in this process's place, and reports how it ended.
///
/// # Invariant
///
/// One command, one console, one exit code: `exec` where the platform has it,
/// and inherited stdio plus the child's code where it does not, so a script that
/// ran this file sees the program it asked for and nothing of the head.
fn take_over(program: &Path, skip: usize) -> Result<u8, String> {
    let mut args: Vec<std::ffi::OsString> = std::env::args_os().skip(1 + skip).collect();
    // `--port` is what SPEC and the README teach; accepting it here keeps one
    // vocabulary for the port instead of two.
    if program
        .file_stem()
        .is_some_and(|stem| stem.to_string_lossy().starts_with("gongga-server"))
    {
        args = bind_from_port(args);
    }
    let mut cmd = std::process::Command::new(program);
    cmd.args(&args);
    // The words typed, as one name: `gongga server --help` has to answer with it.
    let mut spoken: Vec<String> = std::env::args_os()
        .next()
        .map(PathBuf::from)
        .and_then(|path| {
            path.file_stem()
                .map(|stem| stem.to_string_lossy().into_owned())
        })
        .into_iter()
        .collect();
    spoken.extend(
        std::env::args_os()
            .skip(1)
            .take(skip)
            .filter_map(|arg| arg.into_string().ok()),
    );
    if !spoken.is_empty() {
        cmd.env("GONGGA_NAME", spoken.join(" "));
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        let error = cmd.exec();
        Err(format!("running {}: {error}", program.display()))
    }
    #[cfg(not(unix))]
    {
        let status = cmd
            .status()
            .map_err(|e| format!("running {}: {e}", program.display()))?;
        Ok(status.code().unwrap_or(1).clamp(0, 255) as u8)
    }
}
