//! The directory a shell looks in for the harness's own command.
//!
//! See `docs/notes/server-shell-command.md`.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

/// The command layer's own name, the one a shell types.
const CLI: &str = "gongga-cli";

/// A variable naming the command layer when it is somewhere unusual.
const CLI_BIN: &str = "GONGGA_CLI_BIN";

/// Puts the command layer in this home's `bin`, and says where it landed.
///
/// # Invariant
///
/// A shell this server starts must be able to drive it, and this home's `bin` is
/// the only name it has: a launcher or `cargo run` starts a server with nothing
/// beside it, so one is fetched from three named places and never from `PATH`.
/// See `docs/notes/server-shell-command.md`.
pub fn ensure_command(home: &Path) -> Option<PathBuf> {
    let installed = home.join("bin").join(file_name(CLI));
    if installed.is_file() {
        return Some(installed);
    }
    let named = std::env::var_os(CLI_BIN).map(PathBuf::from);
    let source = named
        .into_iter()
        .chain(
            std::env::current_exe()
                .ok()
                .and_then(|me| me.parent().map(|dir| dir.join(file_name(CLI)))),
        )
        .find(|candidate| candidate.is_file())?;
    let bin = home.join("bin");
    std::fs::create_dir_all(&bin).ok()?;
    // Beside it, then renamed in: a shell may be running the old one.
    let temp = bin.join(format!(".{}.{}", file_name(CLI), std::process::id()));
    let _ = std::fs::remove_file(&temp);
    if copy(&source, &temp).is_ok() && std::fs::rename(&temp, &installed).is_ok() {
        return Some(installed);
    }
    let _ = std::fs::remove_file(&temp);
    None
}

/// Says, once, that no command layer could be found and where it looked.
pub fn warn_missing_command(home: &Path) {
    let sibling = std::env::current_exe()
        .ok()
        .and_then(|me| me.parent().map(|dir| dir.join(file_name(CLI))));
    tracing::warn!(
        looked_in = ?[std::env::var_os(CLI_BIN).map(PathBuf::from), sibling],
        "no {CLI} in {}/bin: a shell this server starts cannot drive it",
        home.display()
    );
}

/// The file name a program is started by on this platform.
fn file_name(name: &str) -> String {
    if cfg!(windows) {
        format!("{name}.exe")
    } else {
        name.to_string()
    }
}

/// A link where one can be made, a copy where it cannot cross a volume.
fn copy(from: &Path, to: &Path) -> std::io::Result<()> {
    if std::fs::hard_link(from, to).is_ok() {
        return Ok(());
    }
    std::fs::copy(from, to).map(|_| ())
}

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

    /// A server with no command beside it places one.
    #[test]
    fn a_home_with_no_command_gets_one() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("home");
        std::fs::create_dir_all(&home).unwrap();
        let source = dir.path().join("gongga-cli-source");
        std::fs::write(&source, b"the command layer").unwrap();
        // SAFETY: no other test in this crate reads this variable.
        unsafe { std::env::set_var(CLI_BIN, &source) };
        let placed = ensure_command(&home).expect("a placeable command");
        unsafe { std::env::remove_var(CLI_BIN) };
        assert_eq!(placed, home.join("bin").join(file_name(CLI)));
        assert_eq!(std::fs::read(&placed).unwrap(), b"the command layer");
        // And a second start finds it there rather than placing it twice.
        assert_eq!(ensure_command(&home).unwrap(), placed);
    }
}
