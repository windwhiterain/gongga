//! The directory a shell looks in for the harness's own command.
//!
//! See `docs/notes/server-shell-command.md`.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

/// The `PATH` value that finds a home's `bin` first.
///
/// # Invariant
///
/// The value is POSIX-shaped: a native `C:\a;C:\b` handed to Git Bash is mangled
/// at its startup, and the shell would then find no command at all.
pub fn path(home: &Path) -> String {
    entries(home)
        .iter()
        .map(|entry| shell_path(entry))
        .collect::<Vec<_>>()
        .join(":")
}

/// The same value for a native process: a script's interpreter reads this one.
///
/// # Invariant
///
/// Native shape, because an interpreter given `/c/...` entries loses its own
/// lookups. Both shapes name the same directory, so `gongga` resolves to one file.
pub fn native_path(home: &Path) -> OsString {
    std::env::join_paths(entries(home)).unwrap_or_else(|_| home.join("bin").into_os_string())
}

/// This home's `bin`, then whatever this process inherited, without empty entries.
///
/// SPEC: a shell reads an empty `PATH` entry as "the current directory", and a
/// shell that resolves commands out of wherever it happens to be is an accident
/// waiting for a file named `gongga`.
fn entries(home: &Path) -> Vec<PathBuf> {
    let mut parts = vec![home.join("bin")];
    parts.extend(
        std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default())
            .filter(|entry| !entry.as_os_str().is_empty()),
    );
    parts
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

    /// The home's `bin` is first on the line, so a `gongga` elsewhere -- a build
    /// output, another install -- cannot win the lookup.
    #[test]
    fn the_home_bin_comes_first() {
        let home = Path::new("/h");
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

    /// The same directory, in the shape a script's interpreter reads, so `gongga`
    /// means one file whether a shell or a program looks it up.
    #[test]
    fn the_native_path_names_the_same_directory_in_the_native_shape() {
        let home = tempfile::tempdir().unwrap();
        let native = native_path(home.path());
        // One name in both shapes: the same file answers a shell and a script.
        let first = std::env::split_paths(&native).next().unwrap();
        assert_eq!(first, home.path().join("bin"));
        // And the native shape is the one an interpreter can read: no `/c/...`.
        assert!(
            native
                .to_string_lossy()
                .starts_with(&*home.path().to_string_lossy())
        );
    }
}
