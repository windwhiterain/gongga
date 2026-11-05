//! The directory a shell looks in for the harness's own command.
//!
//! See `docs/notes/server-shell-command.md`.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

/// The command layer's own name, the one a shell types.
const CLI: &str = "gongga-cli";

/// The server's own name, the program this one is.
const SERVER: &str = "gongga-server";

/// The POSIX name: bash runs this file by its `#!`, which a `.cmd` cannot be.
const SH: &str = "#!/bin/sh\n\
# gongga-cli: the harness's command line, in Python. The server writes this file.\n\
here=$(dirname \"$0\")\n\
home=${GONGGA_HOME:-$(cd \"$here/..\" && pwd)}\n\
for candidate in python3 python py; do\n\
  if command -v \"$candidate\" >/dev/null 2>&1; then\n\
    exec \"$candidate\" \"$home/lib/gongga_cli.py\" \"$@\"\n\
  fi\n\
done\n\
echo \"gongga-cli: no Python on this machine's PATH\" >&2\n\
exit 3\n";

/// The native name: `CreateProcess` knows nothing of `#!`.
const CMD: &str = "@echo off\r\n\
if \"%GONGGA_HOME%\"==\"\" set GONGGA_HOME=%~dp0..\r\n\
python \"%GONGGA_HOME%\\lib\\gongga_cli.py\" %*\r\n\
exit /b %ERRORLEVEL%\r\n";

/// The two names a shell may start the command line by, and what each holds.
const SHIM: [(&str, &str); 2] = [(CLI, SH), ("gongga-cli.cmd", CMD)];

/// Puts this server and its command layer in this home's `bin`, and says where
/// the command landed.
///
/// # Invariant
///
/// A shell this server starts must drive *this* server, and this home's `bin` is
/// the only name it has: a launcher starts one with nothing beside it, so both
/// programs come from named places, never from `PATH`. The pair is refreshed when
/// it is not this build, or a handover leaves the next shell a client for a build
/// that is gone. See `docs/notes/server-shell-command.md`.
pub fn ensure_pair(home: &Path) -> Option<PathBuf> {
    let bin = home.join("bin");
    std::fs::create_dir_all(&bin).ok()?;
    // The server first: a command refreshed without one beside it would be a command
    // whose server cannot be found.
    if let Some(source) = server_source() {
        place(&bin, SERVER, &source);
    }
    // The command line is this home's Python: what a shell needs is a name for it.
    for (name, body) in SHIM {
        let _ = write(&bin, name, body);
    }
    Some(bin.join(CLI))
}

/// The server running now: the program that is asking.
fn server_source() -> Option<PathBuf> {
    std::env::current_exe().ok().filter(|me| me.is_file())
}

/// One shim in `bin`, rewritten only when its bytes differ.
///
/// # Invariant
///
/// Beside and renamed in, like a program: a shell reading the name sees one version of
/// it, and the content is the only thing that can be out of date.
fn write(bin: &Path, name: &str, body: &str) -> std::io::Result<()> {
    let installed = bin.join(name);
    if std::fs::read(&installed).ok().as_deref() == Some(body.as_bytes()) {
        return Ok(());
    }
    let temp = bin.join(format!(".{name}.{}", std::process::id()));
    let _ = std::fs::remove_file(&temp);
    std::fs::write(&temp, body)?;
    // `sh` needs the bit; Windows has none to set.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&temp, std::fs::Permissions::from_mode(0o755))?;
    }
    std::fs::rename(&temp, &installed)
}

/// One program in this home's `bin`, linked when it is not already this build.
///
/// # Invariant
///
/// Written beside and renamed in, so no shell ever starts half a program, and a
/// copy that is running now is left alone: the old one still works, and the next
/// start replaces it.
fn place(bin: &Path, name: &str, source: &Path) -> Option<PathBuf> {
    let installed = bin.join(file_name(name));
    if same_build(&installed, source) {
        return Some(installed);
    }
    let temp = bin.join(format!(".{}.{}", file_name(name), std::process::id()));
    let _ = std::fs::remove_file(&temp);
    let placed = copy(source, &temp).and_then(|()| std::fs::rename(&temp, &installed));
    if placed.is_ok() {
        return Some(installed);
    }
    let _ = std::fs::remove_file(&temp);
    // In use, so this start cannot replace it; a usable old one beats none.
    installed.is_file().then_some(installed)
}

/// Whether `installed` is already `source`: length and mtime, as `cli::server`
/// compares them for the same pair.
fn same_build(installed: &Path, source: &Path) -> bool {
    let (Ok(here), Ok(there)) = (std::fs::metadata(installed), std::fs::metadata(source)) else {
        return false;
    };
    here.len() == there.len()
        && here
            .modified()
            .ok()
            .zip(there.modified().ok())
            .is_some_and(|(a, b)| a == b)
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

/// The `PATH` value that finds a home's `bin` first, then the shell's own tools.
///
/// # Invariant
///
/// The value is POSIX-shaped: a native `C:\a;C:\b` handed to Git Bash is mangled
/// at its startup, and the shell would then find no command at all.
pub fn path(home: &Path) -> String {
    let inherited = entries(home);
    // This home's `bin` stays first: nothing here may shadow the command layer.
    let mut parts = vec![skill::shell_path(&inherited[0])];
    parts.extend(SHELL_TOOLS.iter().map(|tool| tool.to_string()));
    parts.extend(
        inherited
            .iter()
            .skip(1)
            .map(|entry| skill::shell_path(entry)),
    );
    // A shell hands its own `PATH` to the next server, which would add these
    // entries again on every handover.
    unique(parts).join(":")
}

/// The directories a shell keeps its own tools in, when `PATH` names none of them.
///
/// # Invariant
///
/// Windows only, in the shell's own spelling: an MSYS root is mounted at `/`, so
/// `/usr/bin` holds the coreutils a native `PATH` never names. On Unix the tools a
/// shell runs are the ones it inherited, and this would put a distribution's copy
/// first. The story is in `docs/notes/server-shell-command.md`.
#[cfg(windows)]
const SHELL_TOOLS: &[&str] = &["/usr/bin", "/bin"];
#[cfg(not(windows))]
const SHELL_TOOLS: &[&str] = &[];

/// The same value for a native process: a script's interpreter reads this one.
///
/// # Invariant
///
/// Native shape, because an interpreter given `/c/...` entries loses its own
/// lookups. Both shapes name the same directory, so `gongga` resolves to one file.
pub fn native_path(home: &Path) -> OsString {
    let parts = unique(
        entries(home)
            .into_iter()
            .map(|entry| entry.to_string_lossy().into_owned())
            .collect(),
    );
    std::env::join_paths(parts).unwrap_or_else(|_| home.join("bin").into_os_string())
}

/// The entries in order, each named once, keeping the first spelling of a name.
///
/// # Invariant
///
/// Two spellings of one directory are one entry on Windows: compared trimmed of
/// its trailing separator and case-folded, because a handover hands the previous
/// shell's `PATH` on and an uncompared list grows by an entry per handover.
fn unique(entries: Vec<String>) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    entries
        .into_iter()
        .filter(|entry| seen.insert(canonical(entry)))
        .collect()
}

/// What makes two spellings of one directory the same name.
fn canonical(entry: &str) -> String {
    let trimmed = match entry.trim_end_matches(['/', '\\']) {
        "" => entry,
        shorter => shorter,
    };
    if cfg!(windows) {
        trimmed.to_lowercase()
    } else {
        trimmed.to_string()
    }
}

/// This home's `bin`, then whatever this process inherited, without empty entries.
///
/// # Invariant
///
/// SPEC: a shell reads an empty `PATH` entry as "the current directory", so a
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
