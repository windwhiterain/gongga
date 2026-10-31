//! The server this program starts: where it is, and how it is started.
//!
//! See `docs/notes/cli-server.md`.

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::Context;

use crate::commands::parse_args;
use crate::{Args, COMMAND, Endpoint, dial};

/// The command that starts a server.
const SERVE: &str = "serve";

/// The program that carries the server, and the variable that names one.
const SERVER: &str = "gongga-server";
const SERVER_BIN: &str = "GONGGA_SERVER_BIN";

/// The log a started server writes, and the only place its refusal is explained.
fn log_path(home: &Path) -> PathBuf {
    home.join("server.log")
}

/// Runs a server in this process's place, in the foreground.
///
/// # Invariant
///
/// Nothing of the client is left running: `serve` was asked for one port on
/// localhost, this home, no link -- and the console that started it belongs to
/// the server from here, exit code included.
pub fn serve(args: &Args, tokens: &[String]) -> anyhow::Result<()> {
    let parsed = parse_args(tokens)?;
    let Some(port) = parsed.get("port") else {
        anyhow::bail!("serve needs --port <port>");
    };
    let port: u16 = port
        .parse()
        .with_context(|| format!("parsing --port {port:?}"))?;
    // `serve --home <dir>` wins over the flag written before the command name:
    // the nearer one is the one the operator just typed.
    let home = match parsed.get("home") {
        Some(named) => crate::home_dir(Some(&PathBuf::from(named))),
        None => crate::home_of(args),
    };
    let server = server_program(&home)?;
    // Before a shell can exist: the command it will type is this program, and the
    // program that serves it sits beside it.
    install_commands(&home, &server);
    let mut cmd = Command::new(&server);
    cmd.arg("--home")
        .arg(&home)
        .arg("--bind")
        .arg(format!("127.0.0.1:{port}"))
        .current_dir(&home);
    take_over(cmd, &server)
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

/// Starts one without asking first, and answers the pid of the process it spawned.
///
/// # Invariant
///
/// The pid is what makes "did *this* window start the server it is talking to"
/// answerable: a home already served by somebody else refuses the lock, so the
/// process started here exits at once and the pid never answers a dial. A caller
/// therefore compares it with the pid the greeting carried rather than trusting
/// the spawn. See `docs/notes/cli-server.md`.
pub fn spawn(home: &Path, addr: SocketAddr) -> anyhow::Result<Option<u32>> {
    if !starts_here(&addr) {
        return Ok(None);
    }
    start(home, addr)
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
fn start(home: &Path, addr: SocketAddr) -> anyhow::Result<Option<u32>> {
    let server = server_program(home)?;
    std::fs::create_dir_all(home).with_context(|| format!("creating {}", home.display()))?;
    // Before the server exists, because the server cannot do it: its own
    // `current_exe()` is `gongga-server`, not what a shell should type.
    install_commands(home, &server);
    let log = log_path(home);
    let out = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log)
        .with_context(|| format!("opening {}", log.display()))?;
    let err = out.try_clone().context("duplicating the log handle")?;
    let mut cmd = Command::new(&server);
    cmd.arg("--home")
        .arg(home)
        .arg("--bind")
        .arg(addr.to_string())
        .current_dir(home)
        .stdin(Stdio::null())
        .stdout(Stdio::from(out))
        .stderr(Stdio::from(err));
    detach(&mut cmd);
    let child = cmd
        .spawn()
        .with_context(|| format!("starting {}", server.display()))?;
    eprintln!(
        "started {SERVE} for {} (pid {}, log {})",
        home.display(),
        child.id(),
        log.display()
    );
    Ok(Some(child.id()))
}

/// Where the program that carries the server is.
///
/// # Invariant
///
/// Three named places, never a search: an explicit `GONGGA_SERVER_BIN`, the
/// sibling of this program (a build tree and an install both hold the pair
/// together), then the copy in this home's `bin`. A `PATH` lookup would start
/// whichever build answered it, which is the skew the install exists to prevent.
fn server_program(home: &Path) -> anyhow::Result<PathBuf> {
    let named = std::env::var_os(SERVER_BIN).map(PathBuf::from);
    find_server(
        home,
        named.as_deref(),
        std::env::current_exe().ok().as_deref(),
    )
}

/// The three places themselves, so a test does not have to be argv[0].
fn find_server(home: &Path, named: Option<&Path>, me: Option<&Path>) -> anyhow::Result<PathBuf> {
    let mut tried = Vec::new();
    let candidates = named
        .map(Path::to_path_buf)
        .into_iter()
        .chain(
            me.and_then(Path::parent)
                .map(|dir| dir.join(program_name(SERVER))),
        )
        .chain([home.join("bin").join(program_name(SERVER))]);
    for candidate in candidates {
        if candidate.is_file() {
            return Ok(candidate);
        }
        tried.push(candidate);
    }
    anyhow::bail!(
        "no `{SERVER}` to start: looked in {tried:?}. Build it (`cargo build -p gongga-server`), \
         set {SERVER_BIN}, or start the server some other way"
    )
}

/// The two files a home's `bin` holds, put there by whoever starts a server.
///
/// # Invariant
///
/// `gongga` is this program and `gongga-server` is the one about to serve, both
/// from one build and refreshed together: a shell reaches the build the server
/// runs, not a copy an operator keeps in step by hand.
fn install_commands(home: &Path, server: &Path) {
    let (Ok(me), Ok(server)) = (std::env::current_exe(), server.canonicalize()) else {
        eprintln!(
            "warning: cannot resolve the programs running now, so neither `{COMMAND}` nor \
             `{SERVER}` can be installed in {}; a shell this home starts would have no command \
             to drive the server with",
            home.display()
        );
        return;
    };
    install_pair(home, &me, &server);
}

/// The pair landing in `<home>/bin`, with both paths already resolved.
///
/// # Invariant
///
/// The server goes in first: a `gongga` refreshed without one beside it is a
/// command whose server cannot be found.
fn install_pair(home: &Path, cli: &Path, server: &Path) {
    for (name, program) in [(SERVER, server), (COMMAND, cli)] {
        if let Err(e) = place(home, name, program) {
            eprintln!(
                "warning: cannot install `{name}` in {}: {e}",
                home.display()
            );
        }
    }
}

/// Puts `program` at `<home>/bin/<name>`, refreshed when it is not this build.
///
/// See `docs/notes/cli-server.md`.
fn place(home: &Path, name: &str, program: &Path) -> std::io::Result<PathBuf> {
    let bin = home.join("bin");
    std::fs::create_dir_all(&bin)?;
    let target = bin.join(program_name(name));
    if this_build(&target, program) {
        return Ok(target);
    }
    // Beside it, then renamed: the command may be running now, and half an
    // executable is worse than yesterday's.
    let temp = bin.join(format!(".{}.{}", program_name(name), std::process::id()));
    let _ = std::fs::remove_file(&temp);
    link_or_copy(program, &temp)?;
    let placed = std::fs::rename(&temp, &target);
    match placed {
        Ok(()) => {
            // One directory holds one answer to the lookup `PATH` does: the other
            // spelling is what an older install left behind.
            if let Some(other) = other_name(name) {
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
fn other_name(name: &str) -> Option<String> {
    (program_name(name) != name).then(|| name.to_string())
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
        && here
            .modified()
            .ok()
            .zip(there.modified().ok())
            .is_some_and(|(a, b)| a == b)
}

/// One program's file name on this platform.
fn program_name(name: &str) -> String {
    if cfg!(windows) {
        format!("{name}.exe")
    } else {
        name.to_string()
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

/// Gives this process's console to `server`, and reports how it ended.
///
/// # Invariant
///
/// `serve` is one command, one console and one exit code: where the platform can
/// replace this process image it does (`exec`), and where it cannot (Windows) the
/// child inherits all three stdio handles and this process only waits. Either way
/// nothing of the client runs alongside the server.
fn take_over(cmd: Command, server: &Path) -> anyhow::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        let mut cmd = cmd;
        let error = cmd.exec();
        Err(error).with_context(|| format!("running {}", server.display()))
    }
    #[cfg(not(unix))]
    {
        let mut cmd = cmd;
        let status = cmd
            .status()
            .with_context(|| format!("running {}", server.display()))?;
        if status.success() {
            Ok(())
        } else {
            // The code matters to a script that started `serve`: it is the
            // server's verdict, not this program's opinion of it.
            std::process::exit(status.code().unwrap_or(1));
        }
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
        let placed = place(&home, COMMAND, &program).unwrap();
        assert_eq!(placed, bin.join(program_name(COMMAND)));
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
        std::fs::write(bin.join(program_name(COMMAND)), b"last week's build").unwrap();
        let placed = place(&home, COMMAND, &program).unwrap();
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
        let placed = place(&home, COMMAND, &program).unwrap();
        let before = std::fs::metadata(&placed).unwrap().modified().unwrap();
        std::thread::sleep(std::time::Duration::from_millis(20));
        place(&home, COMMAND, &program).unwrap();
        assert_eq!(
            std::fs::metadata(&placed).unwrap().modified().unwrap(),
            before
        );
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
        place(&home, COMMAND, &program).unwrap();
        assert!(
            !bin.join(COMMAND).exists(),
            "the old script would win the lookup"
        );
    }

    /// The two files a home ships are one build: `gongga` starts `gongga-server`,
    /// so both arrive together or a shell has a command with no server.
    #[test]
    fn a_home_gets_both_programs_from_one_install() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("home");
        let build = dir.path().join("build");
        std::fs::create_dir_all(&build).unwrap();
        let cli = build.join(program_name(COMMAND));
        let server = build.join(program_name(SERVER));
        std::fs::write(&cli, b"the command").unwrap();
        std::fs::write(&server, b"the server").unwrap();

        install_pair(&home, &cli, &server);

        let bin = home.join("bin");
        assert_eq!(
            std::fs::read(bin.join(program_name(COMMAND))).unwrap(),
            b"the command"
        );
        assert_eq!(
            std::fs::read(bin.join(program_name(SERVER))).unwrap(),
            b"the server"
        );
    }

    /// The server is found where it is named, beside this program, or in the
    /// home -- three answers, never a `PATH` search.
    #[test]
    fn the_server_is_looked_for_in_three_named_places() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("home");
        let build = dir.path().join("build");
        let bin = home.join("bin");
        for made in [&build, &bin] {
            std::fs::create_dir_all(made).unwrap();
        }
        let me = build.join(program_name(COMMAND));
        let named = dir.path().join("named");
        let missing = find_server(&home, Some(&named), Some(&me)).expect_err("nothing is there");
        let message = format!("{missing}");
        assert!(message.contains(SERVER), "{message}");
        assert!(
            message.contains("named"),
            "every place is reported: {message}"
        );

        // Beside this program is the build it came from, before any install.
        let sibling = build.join(program_name(SERVER));
        std::fs::write(&sibling, b"beside").unwrap();
        assert_eq!(find_server(&home, None, Some(&me)).unwrap(), sibling);
        // And nothing beside it falls back to the home's own copy.
        std::fs::remove_file(&sibling).unwrap();
        let installed = bin.join(program_name(SERVER));
        std::fs::write(&installed, b"installed").unwrap();
        assert_eq!(find_server(&home, None, Some(&me)).unwrap(), installed);
        // An explicit name wins over both.
        std::fs::write(&named, b"named").unwrap();
        assert_eq!(find_server(&home, Some(&named), Some(&me)).unwrap(), named);
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
