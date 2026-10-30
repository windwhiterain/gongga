//! The directory a shell looks in for the harness's own command.
//!
//! See `docs/notes/server-shell-command.md`.

use std::path::{Path, PathBuf};

/// The name a shell types for the CLI. Not `cmd`: that is Windows' own shell.
pub const COMMAND: &str = "gongga";

/// Where in a home the command lives.
pub fn command_path(home: &Path) -> PathBuf {
    home.join("bin").join(COMMAND)
}

/// The command a shell in this home will find, in whichever shape the CLI left it.
///
/// # Invariant
///
/// Two shapes are possible -- a script named `gongga` that execs the command
/// layer, or the command layer itself under this platform's program name (see
/// `docs/notes/cli-server.md`) -- and `PATH` finds either. `None` means the CLI
/// has not started here yet.
pub fn command(home: &Path) -> Option<PathBuf> {
    [command_path(home), home.join("bin").join(program_name())]
        .into_iter()
        .find(|path| path.is_file())
}

/// The command layer's own file name on this platform.
fn program_name() -> &'static str {
    if cfg!(windows) { "gongga.exe" } else { "gongga" }
}

/// The `PATH` value that finds a home's `bin` first.
///
/// # Invariant
///
/// The value is POSIX-shaped: a native `C:\a;C:\b` handed to Git Bash is mangled
/// at its startup, and the shell would then find no command at all.
pub fn path(home: &Path) -> String {
    let inherited = std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default())
        .filter(|entry| !entry.as_os_str().is_empty())
        .map(|entry| shell_path(&entry))
        .collect::<Vec<_>>();
    let mut parts = vec![shell_path(&home.join("bin"))];
    parts.extend(inherited);
    parts.join(":")
}

/// One path the way a shell reads it: Git Bash spells a drive as `/c/...`.
fn shell_path(path: &Path) -> String {
    if cfg!(windows) {
        crate::tools::bash::msys_path(path)
    } else {
        path.to_string_lossy().into_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The command is one file in the home, whatever else a build directory holds.
    #[test]
    fn the_command_is_one_name_in_the_home() {
        let home = Path::new("/h");
        assert_eq!(command_path(home), home.join("bin").join("gongga"));
        // First on the line, so a `gongga` elsewhere cannot win the lookup.
        assert!(path(home).starts_with(&shell_path(&home.join("bin"))));
    }

    /// SPEC: a shell starts with an empty `PATH` entry as "the current directory",
    /// so it must not be carried over.
    #[test]
    fn an_empty_path_entry_is_dropped() {
        let home = tempfile::tempdir().unwrap();
        let joined = path(home.path());
        assert!(!joined.contains("::"), "{joined}");
    }

    /// Either shape is the command -- the script, or the program itself under its
    /// own name, which is all a home with only the window installed has.
    #[test]
    fn either_shape_of_the_command_is_the_command() {
        let home = tempfile::tempdir().unwrap();
        assert!(command(home.path()).is_none(), "a home with none has none");
        let bin = home.path().join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        let program = bin.join(program_name());
        std::fs::write(&program, "the program").unwrap();
        assert_eq!(command(home.path()).as_deref(), Some(program.as_path()));
        // The script is what bash looks for first, so it is also what is named.
        let script = bin.join(COMMAND);
        std::fs::write(&script, "#!/bin/sh\n").unwrap();
        assert_eq!(command(home.path()).as_deref(), Some(script.as_path()));
    }
}
