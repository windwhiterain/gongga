//! The server this binary carries: how it is started, and what it needs.
//!
//! See `docs/notes/cli-server.md`.

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::Context;

use crate::commands::parse_args;
use crate::{Args, Endpoint, dial};

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
/// `forward` is the subcommand a shell adds to reach the command layer in this
/// program (`CLI` from the window, nothing from `gongga` itself): the server
/// installs *itself* as the command for its shells, see `install_command`.
///
/// # Invariant
///
/// Nothing of the client is left running: this is the whole process, and it is
/// what `serve` was asked for -- one port on localhost, this home, no link.
pub fn serve(args: &Args, tokens: &[String], forward: Option<&'static str>) -> anyhow::Result<()> {
    let parsed = parse_args(tokens)?;
    let Some(port) = parsed.get("port") else {
        anyhow::bail!("serve needs --port <port>");
    };
    let port: u16 = port
        .parse()
        .with_context(|| format!("parsing --port {port:?}"))?;
    // Before a shell can exist: the command it will type is this program.
    install_command(&crate::home_of(args), forward);
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

/// The command an agent's shell types, put in this home by the server itself.
///
/// # Invariant
///
/// It is *this* program -- whoever started the server -- plus the subcommand that
/// reaches the command layer in it (`CLI` for the window, nothing for `gongga`).
/// Never a search: a machine can hold several builds of these programs, and a home
/// that pointed at whichever one answered `PATH` would hand its shells a different
/// program than the server they are talking to.
fn install_command(home: &Path, forward: Option<&str>) {
    let Ok(program) = std::env::current_exe() else {
        eprintln!(
            "warning: cannot find the program running now, so no `{COMMAND}` can be installed \
             in {}; a shell this home starts would have no command to drive the server with",
            home.display()
        );
        return;
    };
    if let Err(e) = write_shim(home, &program, forward) {
        eprintln!(
            "warning: cannot install `{COMMAND}` in {}: {e}",
            home.display()
        );
    }
}

/// Writes the two-line `exec` a shell reads, through a rename.
///
/// See `docs/notes/cli-server.md`.
fn write_shim(home: &Path, program: &Path, forward: Option<&str>) -> std::io::Result<PathBuf> {
    let bin = home.join("bin");
    std::fs::create_dir_all(&bin)?;
    let target = bin.join(COMMAND);
    // Written beside it, then renamed: a shell may be reading the old script,
    // and half a `#!/bin/sh` is a syntax error.
    let temp = bin.join(format!(".{COMMAND}.{}", std::process::id()));
    // The word that says which half of a program that carries two is meant.
    let forward = forward.map(|word| format!(" {word}")).unwrap_or_default();
    std::fs::write(
        &temp,
        format!(
            "#!/bin/sh\nexec \"{}\"{forward} \"$@\"\n",
            shell_path(program)
        ),
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
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }
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

    /// The command an agent types is a script naming the program that started
    /// the server: no search, no copy, no second name.
    #[test]
    fn the_shim_execs_the_server_s_own_program_by_its_absolute_path() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("home");
        let cli = dir.path().join("gongga");
        let path = write_shim(&home, &cli, None).unwrap();
        assert_eq!(path, home.join("bin").join(COMMAND));
        let text = std::fs::read_to_string(&path).unwrap();
        assert_eq!(
            text,
            format!("#!/bin/sh\nexec \"{}\" \"$@\"\n", shell_path(&cli))
        );
        assert!(!text.contains("\\r"), "a shebang for `sh\\r` is no shebang");
        // Rewriting is how a rebuilt command takes effect, so it must not fail
        // just because the script is already there.
        write_shim(&home, &cli, None).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), text);
    }

    /// A program that only *carries* the command layer gets the word that says
    /// which half of it a shell means.
    #[test]
    fn the_shim_names_the_subcommand_that_reaches_the_command_layer() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("home");
        let tui = dir.path().join("gongga-tui");
        let path = write_shim(&home, &tui, Some(crate::CLI)).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert_eq!(
            text,
            format!("#!/bin/sh\nexec \"{}\" cli \"$@\"\n", shell_path(&tui))
        );
    }

    /// The server installs *itself*: the program a shell in this home gets is the
    /// one running, not whatever a search for `gongga` would have found.
    #[test]
    fn the_command_installed_is_this_program() {
        let dir = tempfile::tempdir().unwrap();
        install_command(dir.path(), None);
        let text = std::fs::read_to_string(dir.path().join("bin").join(COMMAND)).unwrap();
        let me = std::env::current_exe().unwrap();
        assert!(text.contains(&shell_path(&me)), "{text}");
        assert!(text.ends_with("\"$@\"\n"), "{text}");
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
}
