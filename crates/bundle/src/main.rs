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
        .ok_or_else(|| {
            format!(
                "{} carries no programs: this is the head alone, which \
                 `cargo install --git ... gongga-bundle` cannot fill. Install the \
                 programs instead (`cargo install --git ... gongga-cli gongga-server`), \
                 or run scripts/bundle.sh to get a file with them inside",
                me.display()
            )
        })?;
    let programs = payload::programs(&mut file, start)
        .map_err(|e| format!("reading the table of {}: {e}", me.display()))?;
    let (wanted, skip) = wanted_program();
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
    take_over(&dir.join(payload::file_name(&wanted)), skip)
}

/// Which program to run, and how many arguments that choice consumed.
///
/// # Invariant
///
/// The file name wins, because a link cannot carry an argument: a copy named
/// `gongga-server` is the server. Failing that the leading word `tui` asks for
/// the window and is taken off before the program sees it -- opening a window
/// must not mean copying this file under a second name.
fn wanted_program() -> (String, usize) {
    if let Some(name) = by_file_name() {
        return (name, 0);
    }
    let word = std::env::args_os().nth(1);
    if word.is_some_and(|arg| arg == "tui" || arg == "window") {
        return ("gongga-tui".to_string(), 1);
    }
    (DEFAULT.to_string(), 0)
}

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
    let args: Vec<std::ffi::OsString> = std::env::args_os().skip(1 + skip).collect();
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
