//! The server this binary carries: how it is started, and what it needs.
//!
//! See `docs/notes/cli-server.md`.

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::Context;

use crate::commands::parse_args;
use crate::{Args, Endpoint, dial, home_dir};

/// The name a shell types for this crate's own program. Not `cmd`: that is
/// Windows' own shell.
const COMMAND: &str = "gongga";

/// The command that turns this process into a server.
const SERVE: &str = "serve";

/// The log a started server writes, and the only place its refusal is explained.
fn log_path(home: &Path) -> PathBuf {
    home.join("server.log")
}

/// Runs this process as a server, in the foreground.
///
/// # Invariant
///
/// Nothing of the client is left running: this is the whole process, and it is
/// what `serve` was asked for -- one port on localhost, this home, no link.
pub fn serve(args: &Args, tokens: &[String]) -> anyhow::Result<()> {
    let parsed = parse_args(tokens)?;
    let Some(port) = parsed.get("port") else {
        anyhow::bail!("serve needs --port <port>");
    };
    let port: u16 = port
        .parse()
        .with_context(|| format!("parsing --port {port:?}"))?;
    let start = harness::Args {
        home: args.conn.home.clone(),
        bind: Some(format!("127.0.0.1:{port}")),
        ..Default::default()
    };
    harness::main(start)
}

/// Starts a server for `addr` when nothing answers there, and says whether it did.
///
/// See `docs/notes/cli-server.md`.
pub async fn ensure(home: &Path, addr: SocketAddr) -> anyhow::Result<bool> {
    install_command(home);
    if !starts_here(&addr) {
        return Ok(false);
    }
    if answers(home, addr).await {
        return Ok(false);
    }
    start(home, addr)?;
    Ok(true)
}

/// Whether a server on `addr` is this machine's to start.
fn starts_here(addr: &SocketAddr) -> bool {
    addr.ip().is_loopback()
}

/// Asks the address whether it is serving, and hangs up if it is.
///
/// # Invariant
///
/// A refusal counts as an answer: a server that says no is there, and starting
/// another one for the same home would be the split this design refuses.
async fn answers(home: &Path, addr: SocketAddr) -> bool {
    let key = std::fs::read_to_string(home.join("server.key"))
        .ok()
        .map(|text| text.trim().to_string())
        .filter(|key| !key.is_empty());
    let Some(key) = key else {
        return false;
    };
    match dial(Endpoint { addr, key }).await {
        Ok((link, _)) => {
            link.goodbye().await;
            true
        }
        Err(failure) => failure.is_fatal(),
    }
}

/// Starts a server in a process of its own, with its own log.
///
/// # Invariant
///
/// Nothing of this process goes with it: a one-shot command exits in
/// milliseconds, and a server that died with its caller would take the turn it
/// just started down with it.
fn start(home: &Path, addr: SocketAddr) -> anyhow::Result<()> {
    let exe = std::env::current_exe().context("finding this program")?;
    std::fs::create_dir_all(home).with_context(|| format!("creating {}", home.display()))?;
    let log = log_path(home);
    let out = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log)
        .with_context(|| format!("opening {}", log.display()))?;
    let err = out.try_clone().context("duplicating the log handle")?;
    let mut cmd = Command::new(&exe);
    cmd.arg("--home")
        .arg(home)
        .arg(SERVE)
        .arg("--port")
        .arg(addr.port().to_string())
        .current_dir(home)
        .stdin(Stdio::null())
        .stdout(Stdio::from(out))
        .stderr(Stdio::from(err));
    detach(&mut cmd);
    cmd.spawn()
        .with_context(|| format!("starting {} {SERVE}", exe.display()))?;
    eprintln!(
        "started {SERVE} for {} (log {})",
        home.display(),
        log.display()
    );
    Ok(())
}

/// The command an agent's shell types, put in this home for whoever starts a
/// shell out of it.
///
/// # Invariant
///
/// The script carries an absolute path and points at the command layer, never at
/// a terminal front-end: an agent that types `gongga` must not be handed a window.
pub fn install_command(home: &Path) {
    let Some(cli) = cli_binary() else {
        eprintln!(
            "warning: no `{COMMAND}` next to this program or on `PATH`; a shell this home \
             starts would have no command to drive the server with"
        );
        return;
    };
    if let Err(e) = write_shim(home, &cli) {
        eprintln!(
            "warning: cannot install `{COMMAND}` in {}: {e}",
            home.display()
        );
    }
}

/// Where the command layer's own program is: this process, then its sibling,
/// then `PATH`.
fn cli_binary() -> Option<PathBuf> {
    let me = std::env::current_exe().ok()?;
    if me.file_name().is_some_and(|name| name == command_name()) {
        return Some(me);
    }
    let sibling = me.parent()?.join(command_name());
    if sibling.is_file() {
        return Some(sibling);
    }
    // `dev.sh` stages one binary; the build it came from is two levels down.
    let staged = me
        .parent()?
        .join("..")
        .join("target")
        .join("debug")
        .join(command_name());
    if staged.is_file() {
        return Some(staged);
    }
    which::which(command_name()).ok()
}

/// The command's file name on this platform.
fn command_name() -> &'static str {
    if cfg!(windows) { "gongga.exe" } else { "gongga" }
}

/// Writes the two-line `exec` a shell reads, through a rename.
///
/// See `docs/notes/cli-server.md`.
fn write_shim(home: &Path, cli: &Path) -> std::io::Result<PathBuf> {
    let bin = home.join("bin");
    std::fs::create_dir_all(&bin)?;
    let target = bin.join(COMMAND);
    // Written beside it, then renamed: a shell may be reading the old script,
    // and half a `#!/bin/sh` is a syntax error.
    let temp = bin.join(format!(".{COMMAND}.{}", std::process::id()));
    std::fs::write(
        &temp,
        format!("#!/bin/sh\nexec \"{}\" \"$@\"\n", shell_path(cli)),
    )?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&temp, std::fs::Permissions::from_mode(0o755))?;
    }
    std::fs::rename(&temp, &target)?;
    Ok(target)
}

/// One path the way a shell reads it: Git Bash spells a drive as `/c/...`.
fn shell_path(path: &Path) -> String {
    let text = path.to_string_lossy().replace('\\', "/");
    let mut chars = text.chars();
    match (chars.next(), chars.next()) {
        (Some(drive), Some(':')) if drive.is_ascii_alphabetic() => {
            format!("/{}{}", drive.to_ascii_lowercase(), &text[2..])
        }
        _ => text,
    }
}

/// Puts the child outside this process's console and group.
///
/// See `docs/notes/cli-server.md`.
fn detach(cmd: &mut Command) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        /// Never shown. Not `DETACHED_PROCESS`: no console at all means one
        /// window per console child.
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        /// A Ctrl-C in this process's group is not the child's business.
        const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
        cmd.creation_flags(CREATE_NO_WINDOW | CREATE_NEW_PROCESS_GROUP);
    }
    #[cfg(unix)]
    {
        cmd.process_group(0);
    }
}

/// The home this process works on, with the command installed for its shells.
pub fn prepare(args: &Args) -> PathBuf {
    let home = home_dir(args.conn.home.as_deref());
    install_command(&home);
    home
}

#[cfg(test)]
mod tests {
    use super::*;

    fn addr(text: &str) -> SocketAddr {
        text.parse().expect("a literal address")
    }

    /// Only an address on this machine is one this process may bind.
    #[test]
    fn only_a_local_address_is_started_here() {
        assert!(starts_here(&addr("127.0.0.1:9700")));
        assert!(starts_here(&addr("[::1]:9700")));
        // A server on another host is an outage to report, not one to answer by
        // starting a second server for the same home.
        assert!(!starts_here(&addr("10.1.2.3:9700")));
        assert!(!starts_here(&addr("[2001:db8::1]:9700")));
    }

    /// The command an agent types is a script naming the real program, not a
    /// copy of it and not a terminal front-end.
    #[test]
    fn the_shim_execs_the_command_layer_by_its_absolute_path() {
        let dir = tempfile::tempdir().unwrap();
        let cli = dir.path().join("gongga");
        let path = write_shim(&dir.path().join("home"), &cli).unwrap();
        assert_eq!(path, dir.path().join("home").join("bin").join(COMMAND));
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.starts_with("#!/bin/sh\n"), "{text}");
        assert!(text.contains(&shell_path(&cli)), "{text}");
        assert!(!text.contains("\\r"), "a shebang for `sh\\r` is no shebang");
        // Rewriting is how a rebuilt command takes effect, so it must not fail
        // just because the script is already there.
        write_shim(&dir.path().join("home"), &cli).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), text);
    }

    /// A home with no key has no server: a server mints one on the way up.
    #[tokio::test]
    async fn a_home_with_no_key_is_not_answering() {
        let dir = tempfile::tempdir().unwrap();
        assert!(!answers(dir.path(), addr("127.0.0.1:9")).await);
    }
}
