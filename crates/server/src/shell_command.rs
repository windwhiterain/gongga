//! The directory a shell looks in for the harness's own command.
//!
//! See `docs/notes/server-shell-command.md`.

use std::path::{Path, PathBuf};

/// The program running this server, named for `GONGGA_CLI`.
///
/// # Invariant
///
/// It is *this* program, the one `serve` was started as, and not a file in the
/// home or on `PATH`: one machine can hold several builds of the command layer,
/// and a script that ran another would be driving a server it does not share a
/// version with. It is also the form a script can execute directly, which the
/// `#!/bin/sh` name in a home's `bin` is not.
pub fn program() -> Option<PathBuf> {
    std::env::current_exe().ok()
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

    /// `GONGGA_CLI` names this program: one a script can execute itself, rather
    /// than the shell script a home's `bin` holds under the command's name.
    #[test]
    fn the_program_named_is_the_one_running() {
        let program = program().expect("a running process has a path");
        assert_eq!(program, std::env::current_exe().unwrap());
        assert!(program.is_file());
        // On Windows that is what makes it executable: no `sh` needed.
        #[cfg(windows)]
        assert!(
            program
                .extension()
                .is_some_and(|extension| extension.eq_ignore_ascii_case("exe")),
            "{}",
            program.display()
        );
    }
}
