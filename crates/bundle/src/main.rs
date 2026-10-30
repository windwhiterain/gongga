//! The head of a distribution: one file that carries the programs.
//!
//! See `docs/notes/bundle-main.md`.

mod payload;

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
    let me = std::env::current_exe().map_err(|e| format!("finding this file: {e}"))?;
    let mut file =
        std::fs::File::open(&me).map_err(|e| format!("opening {}: {e}", me.display()))?;
    let start = payload::footer_of(&mut file)
        .map_err(|e| format!("reading {}: {e}", me.display()))?
        .ok_or_else(|| format!("{} carries no programs", me.display()))?;
    let programs = payload::programs(&mut file, start)
        .map_err(|e| format!("reading the table of {}: {e}", me.display()))?;
    let wanted = program_name();
    if !programs.iter().any(|p| p.name == wanted) {
        let carried: Vec<&str> = programs.iter().map(|p| p.name.as_str()).collect();
        return Err(format!(
            "this file does not carry {wanted}; it carries {carried:?}"
        ));
    }
    let dir = home().join("bin");
    // Every program, not only the one asked for: `serve` looks for the server
    // beside itself.
    for program in &programs {
        payload::extract(&mut file, program, &dir)
            .map_err(|e| format!("placing {} in {}: {e}", program.name, dir.display()))?;
    }
    take_over(&dir.join(payload::file_name(&wanted)))
}

/// Which program this file was started as, decided by its own file name.
///
/// # Invariant
///
/// The name, not a flag: a copy named `gongga-server` is the server and one with
/// no claim is the command line, so one file can be installed under the name a
/// user types without carrying its own arguments.
fn program_name() -> String {
    std::env::args_os()
        .next()
        .map(PathBuf::from)
        .and_then(|path| {
            path.file_stem()
                .map(|stem| stem.to_string_lossy().into_owned())
        })
        .map(|stem| stem.to_ascii_lowercase())
        .filter(|stem| matches!(stem.as_str(), "gongga-server" | "gongga-tui"))
        .unwrap_or_else(|| DEFAULT.to_string())
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
fn take_over(program: &Path) -> Result<u8, String> {
    let args: Vec<std::ffi::OsString> = std::env::args_os().skip(1).collect();
    let mut cmd = std::process::Command::new(program);
    cmd.args(&args);
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
