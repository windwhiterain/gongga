//! The directory a shell looks in for the harness's own command.
//!
//! See `docs/notes/server-shell-command.md`.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

/// The command layer's own name, the one a shell types.
const CLI: &str = "gongga-cli";

/// The server's own name, the program this one is.
const SERVER: &str = "gongga-server";

/// The name of the program that holds a channel: the third half of the pair.
const RELAY: &str = "gongga-relay";

/// The POSIX name: bash runs this file by its `#!`, which a `.cmd` cannot be.
///
/// # Invariant
///
/// The file it runs is named relative to `GONGGA_HOME`: the shim is written into the
/// home's `bin`, so it expands the home itself, and where the Python surface sits is the
/// content's answer, asked once here rather than spelled twice.
fn sh(contract: &str) -> String {
    let mut text = String::new();
    text.push_str("#!/bin/sh\n");
    text.push_str(
        "# gongga-cli: the harness's command line, in Python. The server writes this file.\n",
    );
    text.push_str("here=$(dirname \"$0\")\n");
    text.push_str("home=${GONGGA_HOME:-$(cd \"$here/..\" && pwd)}\n");
    text.push_str("for candidate in python3 python py; do\n");
    text.push_str("  if command -v \"$candidate\" >/dev/null 2>&1; then\n");
    text.push_str(&format!(
        "    exec \"$candidate\" \"$home/{contract}\" \"$@\"\n"
    ));
    text.push_str("  fi\n");
    text.push_str("done\n");
    text.push_str("echo \"gongga-cli: no Python on this machine's PATH\" >&2\n");
    text.push_str("exit 3\n");
    text
}

/// The native name: `CreateProcess` knows nothing of `#!`.
fn cmd(contract: &str) -> String {
    let windows = contract.replace('/', &char::from(92).to_string());
    let mut text = String::new();
    text.push_str("@echo off\r\n");
    text.push_str("if \"%GONGGA_HOME%\"==\"\" set GONGGA_HOME=%~dp0..\r\n");
    text.push_str(&format!("python \"%GONGGA_HOME%\\{windows}\" %*\r\n"));
    text.push_str("exit /b %ERRORLEVEL%\r\n");
    text
}

/// What the shim runs, relative to the home it starts in.
fn contract(home: &Path) -> String {
    let python = crate::ship::python(home);
    let relative = python.strip_prefix(home).unwrap_or(&python);
    relative
        .join("gongga_cli.py")
        .to_string_lossy()
        .replace('\\', "/")
}

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
    // No relay in `bin` is no channel: a copy it could not replace is asked again.
    refresh_relay(home);
    // The command line is this home's Python: what a shell needs is a name for it.
    let contract = contract(home);
    let _ = write(&bin, CLI, &sh(&contract));
    let _ = write(&bin, "gongga-cli.cmd", &cmd(&contract));
    Some(bin.join(CLI))
}

/// The server running now: the program that is asking.
fn server_source() -> Option<PathBuf> {
    std::env::current_exe().ok().filter(|me| me.is_file())
}

/// Where the relay this server is paired with lives, before it is placed in `bin`.
///
/// # Invariant
///
/// The same three answers, in the same order, as the home's own `bin` lookup: an
/// explicit `GONGGA_RELAY_BIN`, the program beside this one (a build tree and an
/// install both keep the pair together), then nothing -- a home that has one in `bin`
/// needs no source. `Channels` asks this function too, so the two cannot disagree
/// about which relay this server means.
pub(crate) fn relay_source() -> Option<PathBuf> {
    if let Some(named) = std::env::var_os("GONGGA_RELAY_BIN") {
        let named = PathBuf::from(named);
        if named.is_file() {
            return Some(named);
        }
    }
    let beside = std::env::current_exe()
        .ok()
        .and_then(|me| me.parent().map(|dir| dir.join(file_name(RELAY))));
    beside.filter(|path| path.is_file())
}

/// The relay a channel runs: the copy this server installed at startup.
///
/// # Invariant
///
/// A build tree is rebuilt under a server that is still running. The installed copy is
/// the pair this process started with, so a later `cargo build` cannot change the program
/// that holds its channels. It may be a copy `place` could not replace -- a relay still
/// holding a channel pins the file -- and such a copy is what [`refresh_relay`] answers.
pub(crate) fn relay_for_channel(home: &Path) -> PathBuf {
    static RESOLVED: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();
    RESOLVED.get_or_init(|| resolve_relay(home)).clone()
}

/// This process's relay: this build's bytes, under a name no older build asks for when a
/// running copy holds the plain one.
///
/// # Invariant
///
/// A copy that is not this build is never run. SPEC's "validate" is what makes a pair a
/// pair: a stale relay reads this server's `cmd` with last week's shape and fails in a way
/// nobody can diagnose. Resolved once, so a rebuild under a running server cannot change
/// the program that holds its channels. See `docs/notes/bindir.md`.
fn resolve_relay(home: &Path) -> PathBuf {
    if let Some(named) = std::env::var_os("GONGGA_RELAY_BIN") {
        let named = PathBuf::from(named);
        if named.is_file() {
            return named;
        }
    }
    let bin = home.join("bin");
    let Some(source) = relay_source() else {
        return bin.join(file_name(RELAY));
    };
    match bindir::install(&bin, RELAY, bindir::Source::File(&source)) {
        // A running copy holds the name: this build's bytes go beside it, so the pair
        // stays a pair and the copy an older server started is left to finish.
        Ok(bindir::Outcome::Pinned { installed, why }) => {
            tracing::warn!(
                installed = %installed.display(),
                why,
                "the relay's name is held by a running copy; this build's copy goes beside it"
            );
            bindir::install_aside(&bin, RELAY, bindir::Source::File(&source)).unwrap_or(source)
        }
        Ok(outcome) => outcome.path().to_path_buf(),
        Err(e) => {
            tracing::warn!(error = %e, "could not install this build's relay; running the one beside this program");
            source
        }
    }
}

/// This build's relay in `bin`, updated when it is behind; `None` if there is no source.
///
/// # Invariant
///
/// A copy that could not be replaced is not an update: this answers `Some` only when
/// `bin` holds the source's own bytes, so a caller asking after a refusal can tell "this
/// build now" from "still the one that refused".
pub(crate) fn refresh_relay(home: &Path) -> Option<PathBuf> {
    let bin = home.join("bin");
    let installed = bin.join(file_name(RELAY));
    let source = relay_source()?;
    // Never a source for itself: a server run from `bin` would otherwise "update" the
    // file with itself.
    if same_file(&source, &installed) {
        return None;
    }
    std::fs::create_dir_all(&bin).ok()?;
    place(&bin, RELAY, &source)?;
    bindir::holds(&bin, RELAY, bindir::Source::File(&source)).then_some(installed)
}

/// Whether two paths name one file.
fn same_file(left: &Path, right: &Path) -> bool {
    match (std::fs::canonicalize(left), std::fs::canonicalize(right)) {
        (Ok(left), Ok(right)) => left == right,
        _ => false,
    }
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
    match bindir::install(bin, name, bindir::Source::File(source)) {
        Ok(bindir::Outcome::Pinned { installed, why }) => {
            tracing::warn!(
                name,
                installed = %installed.display(),
                why,
                "a running copy holds this program's name; the directory keeps it"
            );
            Some(installed)
        }
        Ok(outcome) => Some(outcome.path().to_path_buf()),
        Err(e) => {
            tracing::warn!(name, error = %e, "could not install this program in the home's bin");
            None
        }
    }
}

/// The file name a program is started by on this platform.
fn file_name(name: &str) -> String {
    if cfg!(windows) {
        format!("{name}.exe")
    } else {
        name.to_string()
    }
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
