//! The server this binary carries: how it is started, and what it needs.
//!
//! See `docs/notes/cli-server.md`.

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::Context;

use crate::commands::parse_args;
use crate::{Args, COMMAND, Endpoint, dial};

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
    // Before a shell can exist: the command it will type is this program.
    install_command(&crate::home_of(args));
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

/// The command an agent's shell and its scripts both run, put in this home by the
/// server itself.
///
/// # Invariant
///
/// It is *this* program -- whoever started the server -- under the name a shell
/// types, and it is a link rather than a wrapper script: `bash` resolves
/// `gongga` to `gongga.exe` by itself, and a native process starting `gongga`
/// finds the same file, so one name works on both sides of the shell boundary.
/// Never a search: a machine can hold several builds of these programs, and a
/// home that pointed at whichever one answered `PATH` would hand its shells a
/// different program than the server they are talking to.
fn install_command(home: &Path) {
    let Ok(program) = std::env::current_exe() else {
        eprintln!(
            "warning: cannot find the program running now, so no `{COMMAND}` can be installed \
             in {}; a shell this home starts would have no command to drive the server with",
            home.display()
        );
        return;
    };
    if let Err(e) = place(home, &program) {
        eprintln!(
            "warning: cannot install `{COMMAND}` in {}: {e}",
            home.display()
        );
    }
}

/// Puts `program` at `<home>/bin/gongga`, refreshed when it is not this build.
///
/// See `docs/notes/cli-server.md`.
fn place(home: &Path, program: &Path) -> std::io::Result<PathBuf> {
    let bin = home.join("bin");
    std::fs::create_dir_all(&bin)?;
    let target = bin.join(command_name());
    if this_build(&target, program) {
        return Ok(target);
    }
    // Beside it, then renamed: the command may be running right now, and a
    // half-written executable is worse than yesterday's.
    let temp = bin.join(format!(".{}.{}", command_name(), std::process::id()));
    let _ = std::fs::remove_file(&temp);
    link_or_copy(program, &temp)?;
    let placed = std::fs::rename(&temp, &target);
    match placed {
        Ok(()) => {
            // One directory holds one answer to the lookup `PATH` does: the other
            // spelling is what an older install left behind.
            if let Some(other) = other_name() {
                let _ = std::fs::remove_file(bin.join(other));
            }
            Ok(target)
        }
        Err(e) => {
            // A running command cannot be replaced on Windows; the old one stays
            // until the next start, which is what a stale build means anyway.
            let _ = std::fs::remove_file(&temp);
            Err(e)
        }
    }
}

/// The spelling this platform does not use, if the other one is possible at all.
fn other_name() -> Option<&'static str> {
    (command_name() != COMMAND).then_some(COMMAND)
}

/// Whether `target` is already the program running now.
///
/// # Invariant
///
/// Size and modification time, because a rebuild replaces the file: a hard link
/// then still names yesterday's bytes, while a symlink follows the path by itself.
fn this_build(target: &Path, program: &Path) -> bool {
    let (Ok(here), Ok(there)) = (std::fs::metadata(target), std::fs::metadata(program)) else {
        return false;
    };
    here.len() == there.len()
        && here.modified().ok().zip(there.modified().ok()).is_some_and(|(a, b)| a == b)
}

/// The command's file name on this platform.
fn command_name() -> &'static str {
    if cfg!(windows) {
        "gongga.exe"
    } else {
        COMMAND
    }
}

/// A link where one can be made, a copy only where it cannot.
///
/// # Invariant
///
/// A hard link cannot cross a volume, and a home may sit on another drive than the
/// program that serves it; the copy is the fallback that keeps the name working,
/// at the price of going stale on the next rebuild like any other copy.
fn link_or_copy(program: &Path, target: &Path) -> std::io::Result<()> {
    if link(program, target).is_ok() {
        return Ok(());
    }
    std::fs::copy(program, target).map(|_| ())
}

#[cfg(windows)]
fn link(program: &Path, target: &Path) -> std::io::Result<()> {
    std::fs::hard_link(program, target)
}

/// A symlink, so a rebuild is picked up without this module noticing.
#[cfg(unix)]
fn link(program: &Path, target: &Path) -> std::io::Result<()> {
    std::os::unix::fs::symlink(program, target)
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

    /// The command a shell and a script both run is this program, under the name
    /// they type: one file with two names, not a second copy of it.
    #[test]
    fn the_command_is_this_program_under_the_shell_s_name() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("home");
        let bin = home.join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        let program = bin.join("program");
        std::fs::write(&program, b"the program").unwrap();
        let placed = place(&home, &program).unwrap();
        assert_eq!(placed, bin.join(command_name()));
        assert_eq!(std::fs::read(&placed).unwrap(), b"the program");
        // The same bytes, once: a link is the point.
        assert_eq!(
            std::fs::metadata(&placed).unwrap().len(),
            std::fs::metadata(&program).unwrap().len()
        );
    }

    /// A rebuild replaces the program file, so the name has to be replaced with
    /// it -- a last week's build answering to `gongga` is the skew this avoids.
    #[test]
    fn a_stale_command_is_replaced_by_the_one_running() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("home");
        let bin = home.join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        let program = bin.join("program");
        std::fs::write(&program, b"this build").unwrap();
        // A different build is a different size; the timestamp alone is a weaker
        // signal than the bytes.
        std::fs::write(bin.join(command_name()), b"last week's build").unwrap();
        let placed = place(&home, &program).unwrap();
        assert_eq!(std::fs::read(&placed).unwrap(), b"this build");
    }

    /// Nothing is rewritten when the name already carries this build: the home is
    /// not churn on every start.
    #[test]
    fn a_current_command_is_left_in_place() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("home");
        let bin = home.join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        let program = bin.join("program");
        std::fs::write(&program, b"this build").unwrap();
        let placed = place(&home, &program).unwrap();
        let before = std::fs::metadata(&placed).unwrap().modified().unwrap();
        std::thread::sleep(std::time::Duration::from_millis(20));
        place(&home, &program).unwrap();
        assert_eq!(std::fs::metadata(&placed).unwrap().modified().unwrap(), before);
    }

    /// The other spelling is what an older install left there, and it is what a
    /// shell would find first: one name, one directory.
    #[cfg(windows)]
    #[test]
    fn the_other_spelling_is_removed() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("home");
        let bin = home.join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        let program = bin.join("program");
        std::fs::write(&program, b"the program").unwrap();
        std::fs::write(bin.join(COMMAND), "#!/bin/sh\n").unwrap();
        place(&home, &program).unwrap();
        assert!(!bin.join(COMMAND).exists(), "the old script would win the lookup");
    }

    /// Where a link cannot be made -- another volume -- a copy keeps the name
    /// working. A target that is already there is the cheapest way to make the
    /// link fail here.
    #[test]
    fn a_link_that_fails_falls_back_to_a_copy() {
        let dir = tempfile::tempdir().unwrap();
        let program = dir.path().join("program");
        std::fs::write(&program, b"the program").unwrap();
        let taken = dir.path().join("already-there");
        std::fs::write(&taken, b"old").unwrap();
        link_or_copy(&program, &taken).unwrap();
        assert_eq!(std::fs::read(&taken).unwrap(), b"the program");
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
