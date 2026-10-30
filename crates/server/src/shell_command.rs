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

/// The command a shell in this home will find, if the CLI has started here yet.
///
/// The file is a script the program that served this home wrote, naming itself
/// (`docs/notes/cli-server.md`): a name, not a search, is what a shell gets.
pub fn command(home: &Path) -> Option<PathBuf> {
    let path = command_path(home);
    path.is_file().then_some(path)
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

    /// A home with no command says so; one with the script names the script.
    #[test]
    fn the_command_is_the_script_or_nothing() {
        let home = tempfile::tempdir().unwrap();
        assert!(command(home.path()).is_none(), "a home with none has none");
        let bin = home.path().join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        std::fs::write(bin.join(COMMAND), "#!/bin/sh\n").unwrap();
        assert_eq!(
            command(home.path()).as_deref(),
            Some(command_path(home.path()).as_path())
        );
    }
}
