//! The harness's own command, installed where a shell can name it.
//!
//! See `docs/notes/server-shell-command.md`.

use std::path::Path;

use anyhow::Context;

/// The name a shell types for the CLI. Not `cmd`: that is Windows' own shell.
const COMMAND: &str = "gongga";

/// Writes `<home>/bin/gongga` and returns the `PATH` value that finds it.
///
/// # Invariant
///
/// The script carries the CLI's absolute path and the `PATH` is POSIX-shaped.
/// A native `C:\a;C:\b` handed to Git Bash is mangled at its startup, and the
/// shell would then find no command at all.
pub fn install(home: &Path, cli: &Path) -> anyhow::Result<String> {
    let bin = home.join("bin");
    let script = format!("#!/bin/sh\nexec \"{}\" \"$@\"\n", shell_path(cli));
    std::fs::create_dir_all(&bin)
        .with_context(|| format!("creating {} to hold the `{COMMAND}` command", bin.display()))?;
    std::fs::write(bin.join(COMMAND), &script)
        .with_context(|| format!("writing the `{COMMAND}` command for {}", cli.display()))?;
    // MSYS treats every file on NTFS as runnable; a real filesystem does not.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(bin.join(COMMAND), std::fs::Permissions::from_mode(0o755))
            .with_context(|| format!("making the `{COMMAND}` command runnable"))?;
    }
    Ok(path_with(&bin))
}

/// Our bin first, then everything this process inherited, each entry in the
/// shape a shell started by this server reads.
fn path_with(bin: &Path) -> String {
    let inherited = std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default())
        .filter(|entry| !entry.as_os_str().is_empty())
        .map(|entry| shell_path(&entry))
        .collect::<Vec<_>>();
    let mut parts = vec![shell_path(bin)];
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
