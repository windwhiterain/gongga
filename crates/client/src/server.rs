//! The server this program starts: where it is, and how it is started.
//!
//! See `docs/notes/cli-server.md`.

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

use anyhow::Context;

use crate::commands::parse_args;
use crate::{Args, COMMAND, Connection, Endpoint, dial};

/// The command that starts a server.
const SERVE: &str = "serve";

/// The program that carries the server, and the variable that names one.
const SERVER: &str = "gongga-server";
const SERVER_BIN: &str = "GONGGA_SERVER_BIN";
/// The peer-side program a session's shells run under, when a build has one.
const RUNNER: &str = "gongga-runner";

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
    // SPEC: `serve <port> [--home <path>]`, from the same published spec a
    // front-end completes against.
    let spec = protocol::client_commands()
        .into_iter()
        .find(|c| c.name == "serve");
    let parsed = parse_args(tokens, spec.as_ref())?;
    let Some(port) = parsed.get("port") else {
        anyhow::bail!("serve needs a port: `{} serve <port>`", crate::typed_name());
    };
    let port: u16 = port
        .parse()
        .with_context(|| format!("parsing port {port:?}"))?;
    // The flag before the command name loses to `serve --home`: nearer is newer.
    let home = match parsed.get("home") {
        Some(named) => crate::home_dir(Some(&PathBuf::from(named))),
        None => crate::home_of(args),
    };
    let server = server_program(&home)?;
    // Before a shell can exist: the command it will type is this program, and the
    // program that serves it sits beside it.
    install_commands(&home, &server);
    let cmd = server_command(&server, &home, &format!("127.0.0.1:{port}"));
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
    let pid = start(home, addr, None, None)?;
    // Only here: `ensure` is the one-shot path, whose stderr is being read.
    if let Some(pid) = pid {
        eprintln!(
            "started {SERVE} for {} (pid {pid}, log {})",
            home.display(),
            log_path(home).display()
        );
    }
    Ok(true)
}

/// Starts one without asking first, and answers the pid of the process it spawned.
///
/// # Invariant
///
/// The pid answers "did this window start the server it is talking to": it is the
/// watchdog's when one runs, where a greeting carries the server's.
///
/// # Invariant
///
/// `key` is what this process dials with, so a server may not mint its own; `None` lets
/// the home decide. See `docs/notes/cli-server.md`.
pub fn spawn(
    home: &Path,
    addr: SocketAddr,
    owner: Option<u32>,
    key: Option<&str>,
) -> anyhow::Result<Option<u32>> {
    if !starts_here(&addr) {
        return Ok(None);
    }
    start(home, addr, owner, key)
}

/// How long a server is given to honour `cancel` before it is killed instead.
const CANCEL_GRACE: Duration = Duration::from_secs(5);
/// How often the process is asked whether it is gone.
const GONE_POLL: Duration = Duration::from_millis(100);

/// Stops whatever serves `addr`, so this process can start its own there.
///
/// # Invariant
///
/// Asked first, killed only if it will not answer; returns the pid that was serving.
///
/// # Invariant
///
/// The key is the one this process would *dial* with, never the home's: a client started
/// with `--key` proves it by using it. See `docs/notes/cli-server.md`.
pub async fn stop(
    conn: &Connection,
    home: &Path,
    addr: SocketAddr,
) -> anyhow::Result<Option<u32>> {
    if !starts_here(&addr) {
        return Ok(None);
    }
    let Some(key) = crate::resolve_key(conn).or_else(|| crate::key_of(home)) else {
        return Ok(None);
    };
    let Some((mut link, greeting)) = dial(Endpoint { addr, key }).await.ok() else {
        return Ok(None);
    };
    let pid = greeting.pid();
    link.cancel().await;
    if waits_out(pid, CANCEL_GRACE).await {
        return Ok(Some(pid));
    }
    // The watchdog first: a server killed under a live one is started again.
    let supervisor = greeting.supervisor();
    if let Some(supervisor) = supervisor {
        kill(supervisor);
    }
    kill(pid);
    if waits_out(pid, CANCEL_GRACE).await {
        return Ok(Some(pid));
    }
    // The address is free and the lock is not: a server that survives this is
    // still holding `server.lock`, and the one this caller is about to start
    // reads it as a live server and refuses. Answering `Some` here is what made
    // a swap look finished while the home had no server at all.
    anyhow::bail!(
        "the server on {addr} is still there after cancel and kill (pid {pid}); \
         it holds this home's lock, so a new server would refuse to start"
    )
}

/// Whether `pid` is gone before `grace` runs out.
async fn waits_out(pid: u32, grace: Duration) -> bool {
    let deadline = tokio::time::Instant::now() + grace;
    loop {
        if !alive(pid) {
            return true;
        }
        if tokio::time::Instant::now() >= deadline {
            return false;
        }
        tokio::time::sleep(GONE_POLL).await;
    }
}

/// Whether `pid` is a live process.
///
/// # Invariant
///
/// A pid is not a process: one that has been reused answers yes and would keep a
/// stale server's place forever. Asking the machine is the only honest answer, so
/// a failure to ask counts as alive -- the kill below is what resolves it.
#[cfg(windows)]
pub fn alive(pid: u32) -> bool {
    let mut query = std::process::Command::new("tasklist");
    query.args(["/NH", "/FI", &format!("PID eq {pid}")]);
    console::apply(&mut query, 0);
    query
        .output()
        .map(|out| String::from_utf8_lossy(&out.stdout).contains(&pid.to_string()))
        .unwrap_or(true)
}

#[cfg(not(windows))]
pub fn alive(pid: u32) -> bool {
    std::process::Command::new("kill")
        .args(["-0", &pid.to_string()])
        .status()
        .is_ok_and(|status| status.success())
}

/// Ends `pid` and everything it started.
///
/// # Invariant
///
/// The tree, not the process: a server's shells are its children, and one left
/// behind holds the console it came from.
#[cfg(windows)]
fn kill(pid: u32) -> bool {
    let mut kill = std::process::Command::new("taskkill");
    kill.args(["/PID", &pid.to_string(), "/T", "/F"]);
    console::apply(&mut kill, 0);
    kill.output().is_ok_and(|out| out.status.success())
}

#[cfg(not(windows))]
fn kill(pid: u32) -> bool {
    std::process::Command::new("kill")
        .args(["-9", &pid.to_string()])
        .output()
        .is_ok_and(|out| out.status.success())
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
fn start(
    home: &Path,
    addr: SocketAddr,
    owner: Option<u32>,
    key: Option<&str>,
) -> anyhow::Result<Option<u32>> {
    let server = server_program(home)?;
    std::fs::create_dir_all(home).with_context(|| format!("creating {}", home.display()))?;
    // Before the server exists: its `current_exe()` is not what a shell should type.
    install_commands(home, &server);
    let log = log_path(home);
    let out = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log)
        .with_context(|| format!("opening {}", log.display()))?;
    let err = out.try_clone().context("duplicating the log handle")?;
    let mut cmd = command(&server, home, addr, owner);
    if let Some(key) = key {
        // The variable the server reads; a flag each way would be one spelling
        // more than the pair needs.
        cmd.env(crate::KEY_ENV, key);
    }
    cmd.stdin(Stdio::null())
        .stdout(Stdio::from(out))
        .stderr(Stdio::from(err));
    detach(&mut cmd);
    let child = cmd
        .spawn()
        .with_context(|| format!("starting {}", server.display()))?;
    // Silent: a window that starts its own server owns the screen, and a line on
    // stderr lands in the middle of it.
    Ok(Some(child.id()))
}

/// What to start, before where its output goes.
fn command(server: &Path, home: &Path, addr: SocketAddr, owner: Option<u32>) -> Command {
    let mut cmd = server_command(server, home, &addr.to_string());
    if let Some(owner) = owner {
        // Nobody is left to cancel it once the window is gone; a window of
        // another build is not followed at all.
        cmd.arg("--parent").arg(owner.to_string());
        cmd.arg("--parent-build").arg(protocol::BUILD);
    }
    cmd
}

/// A server for `home` on `bind`, in the directory this process runs in.
///
/// # Invariant
///
/// Never `current_dir`: the child inherits where its starter runs, so a session
/// with no `--cwd` opens where the operator launched the window, not in the home
/// the binary was installed in. See `docs/notes/cli-server.md`.
fn server_command(server: &Path, home: &Path, bind: &str) -> Command {
    let mut cmd = Command::new(server);
    cmd.arg("--home").arg(home).arg("--bind").arg(bind);
    cmd
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
    install_runner(home, &me);
}

/// The runner beside this program, kept in `<home>/bin` so it can be carried to a peer.
///
/// # Invariant
///
/// Absent is normal: a build of the command layer alone has no runner, and a
/// target whose `runner` names a path on the peer never reads this copy. The
/// bundle carries all four programs, which is what puts one here to copy.
fn install_runner(home: &Path, me: &Path) {
    let Some(sibling) = me.parent().map(|dir| dir.join(program_name(RUNNER))) else {
        return;
    };
    if !sibling.is_file() {
        return;
    }
    if let Err(e) = place(home, RUNNER, &sibling) {
        eprintln!(
            "warning: cannot install `{RUNNER}` in {}: {e}; a session on a remote target needs \
             that program on the peer",
            home.join("bin").display()
        );
    }
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
            // A copy in use is not worth a red line; missing is the case to report.
            if home.join("bin").join(program_name(name)).is_file() {
                continue;
            }
            eprintln!(
                "warning: cannot install `{name}` in {}: {e}",
                home.join("bin").display()
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
            // A running command cannot be replaced on Windows; the old one stays.
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
        /// A Ctrl-C in this process's group is not the child's business.
        const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
        console::apply(cmd, CREATE_NEW_PROCESS_GROUP);
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }
}
