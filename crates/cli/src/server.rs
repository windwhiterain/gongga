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
pub fn starts_here(addr: &SocketAddr) -> bool {
    addr.ip().is_loopback()
}

/// Asks the address whether it is serving, and hangs up if it is.
///
/// # Invariant
///
/// A refusal counts as an answer: a server that says no is there, and starting
/// another one for the same home would be the split this design refuses.
async fn answers(home: &Path, addr: SocketAddr) -> bool {
    let Some(key) = crate::key_of(home) else {
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
/// What lands here is the command layer and never a terminal front-end: an agent
/// that types `gongga` must not be handed a window. When the only program
/// around is the window, it is installed *under the command's name* -- the same
/// file, answering to a second name, which is the whole reason this crate can be
/// carried by a front-end (see `docs/notes/tui-main.md`).
pub fn install_command(home: &Path) {
    let Some(program) = cli_binary() else {
        eprintln!(
            "warning: cannot find the program running now, so no `{COMMAND}` can be installed \
             in {}; a shell this home starts would have no command to drive the server with",
            home.display()
        );
        return;
    };
    let installed = if is_command(&program) {
        write_shim(home, &program)
    } else {
        write_named(home, &program)
    };
    if let Err(e) = installed {
        eprintln!(
            "warning: cannot install `{COMMAND}` in {}: {e}",
            home.display()
        );
    }
}

/// Where the command layer's own program is: this process if it already answers
/// to that name, then its sibling, then `PATH`, then this process anyway.
fn cli_binary() -> Option<PathBuf> {
    let me = std::env::current_exe().ok()?;
    if is_command(&me) {
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
    // Last: whatever is running. A program that carries the command layer under
    // another name is still the command layer when it is started as one.
    which::which(command_name()).ok().or(Some(me))
}

/// The command's file name on this platform.
fn command_name() -> &'static str {
    if cfg!(windows) { "gongga.exe" } else { "gongga" }
}

/// Whether this program is the command layer, by the name it answers to.
fn is_command(program: &Path) -> bool {
    program
        .file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.eq_ignore_ascii_case(command_name()))
}

/// Writes the two-line `exec` a shell reads, through a rename.
///
/// See `docs/notes/cli-server.md`.
fn write_shim(home: &Path, cli: &Path) -> std::io::Result<PathBuf> {
    let bin = home.join("bin");
    std::fs::create_dir_all(&bin)?;
    let target = bin.join(COMMAND);
    // One directory holds one answer to the lookup it does. The other shape is
    // only possible where a program's name carries an extension.
    if command_name() != COMMAND {
        let _ = std::fs::remove_file(bin.join(command_name()));
    }
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

/// Installs a program that is not named `gongga` *under* that name: the same
/// file, second name -- which is how a program that only carries the command
/// layer can still be one.
///
/// See `docs/notes/cli-server.md`.
fn write_named(home: &Path, program: &Path) -> std::io::Result<PathBuf> {
    let bin = home.join("bin");
    std::fs::create_dir_all(&bin)?;
    let target = bin.join(command_name());
    if command_name() != COMMAND {
        let _ = std::fs::remove_file(bin.join(COMMAND));
    }
    // Already this program: a link is the same file, and a symlink always is.
    if current(&target, program) {
        return Ok(target);
    }
    let _ = std::fs::remove_file(&target);
    link_or_copy(program, &target)?;
    Ok(target)
}

/// Whether `installed` is already `program`, byte for byte and build for build.
fn current(installed: &Path, program: &Path) -> bool {
    let (Ok(installed), Ok(program)) = (std::fs::metadata(installed), std::fs::metadata(program))
    else {
        return false;
    };
    // A hard link shares the file, so a rebuilt program has a newer one and this
    // is how a stale link is noticed.
    installed.len() == program.len() && installed.modified().ok() == program.modified().ok()
}

/// Puts `program` at `target`, by the cheapest means the platform offers.
#[cfg(windows)]
fn link_or_copy(program: &Path, target: &Path) -> std::io::Result<()> {
    // A hard link needs no privilege and copies nothing; a copy is the answer
    // only when the two are on different volumes.
    std::fs::hard_link(program, target).or_else(|_| std::fs::copy(program, target).map(|_| ()))
}

/// Puts `program` at `target`, by the cheapest means the platform offers.
#[cfg(unix)]
fn link_or_copy(program: &Path, target: &Path) -> std::io::Result<()> {
    // A symlink follows a rebuild by itself; a hard link copies nothing; a copy
    // is what is left when the two are on different volumes.
    std::os::unix::fs::symlink(program, target)
        .or_else(|_| std::fs::hard_link(program, target))
        .or_else(|_| std::fs::copy(program, target).map(|_| ()))
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

    /// A key in `config.toml` is a key: `server.key` is where a *minted* one
    /// goes, not the only place a home can keep one. Reading only the file made
    /// `answers` say no to a server that was there, so every start spawned a
    /// second one to be refused by the lock.
    #[test]
    fn the_key_a_home_carries_in_its_config_is_the_one_to_ask_with() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("config.toml"),
            "[server]\nkey = \"c0ffee\"\n",
        )
        .unwrap();
        assert_eq!(crate::key_from_home(dir.path()).as_deref(), Some("c0ffee"));
    }

    /// A program that only *carries* the command layer is installed under the
    /// command's own name: that name is how it answers as the command layer.
    #[test]
    fn a_program_with_another_name_is_installed_under_the_command_s() {
        let dir = tempfile::tempdir().unwrap();
        let program = dir.path().join("gongga-tui");
        std::fs::write(&program, "the window").unwrap();
        let home = dir.path().join("home");
        let path = write_named(&home, &program).unwrap();
        assert_eq!(path.file_name().unwrap(), command_name());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "the window");
        // Nothing to do the second time, and the link has to survive that.
        write_named(&home, &program).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "the window");
    }

    /// A rebuild replaces the file, leaving the link naming the old one -- and a
    /// link nothing refreshes is a shell running last week's command layer.
    #[test]
    fn a_rebuilt_program_replaces_the_link_that_named_the_old_one() {
        let dir = tempfile::tempdir().unwrap();
        let program = dir.path().join("gongga-tui");
        std::fs::write(&program, "v1").unwrap();
        let home = dir.path().join("home");
        let path = write_named(&home, &program).unwrap();
        // The way a build writes: a new file, not through the old one.
        std::fs::remove_file(&program).unwrap();
        std::fs::write(&program, "second build").unwrap();
        assert!(!current(&path, &program), "a stale link looked current");
        write_named(&home, &program).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "second build");
    }

    /// Only the command's own name, in either spelling, is the command layer.
    #[test]
    fn another_program_is_not_the_command_layer() {
        if cfg!(windows) {
            assert!(is_command(std::path::Path::new("gongga.exe")));
            assert!(is_command(std::path::Path::new(r"C:\h\bin\GONGGA.EXE")));
            // The script in `bin` is a shim for it, not the program itself.
            assert!(!is_command(std::path::Path::new("gongga")));
        } else {
            assert!(is_command(std::path::Path::new("/h/bin/gongga")));
        }
        assert!(!is_command(std::path::Path::new("gongga-tui")));
        assert!(!is_command(std::path::Path::new("gongga-tui.exe")));
    }
}
