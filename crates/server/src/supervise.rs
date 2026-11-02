//! The process that keeps the server running: one supervisor, one child.
//!
//! See `docs/notes/server-supervise.md`.

use std::ffi::OsString;
use std::process::{Command, ExitStatus};
use std::time::{Duration, Instant};

/// Set for the child: this process is the server, not its supervisor.
pub const SUPERVISED: &str = "GONGGA_SUPERVISED";

/// The supervisor's pid, for the greeting a client reads.
pub const SUPERVISOR: &str = "GONGGA_SUPERVISOR";

/// What a restarted child finds its predecessor's exit in, as `<code>@<millis>`.
pub const LAST_EXIT: &str = "GONGGA_LAST_EXIT";

/// The code a server exits with when it has already said why it stopped.
///
/// # Invariant
///
/// A refusal is not a crash: the supervisor restarts a process that died without
/// explaining itself and leaves a process that explained itself alone. Every exit
/// of `gongga_server::main` that carries an error is this code.
pub const REFUSED: i32 = 2;

/// The exit code of a second, insistent stop request.
const INSISTED: i32 = 130;

/// The first wait before a restart, doubling up to [`MAX_BACKOFF`].
const MIN_BACKOFF: Duration = Duration::from_millis(200);
const MAX_BACKOFF: Duration = Duration::from_secs(30);

/// How long a child has to live to count as healthy, which resets the backoff.
///
/// # Invariant
///
/// A restart that worked is not a crash loop, however many times it happened: a
/// server killed once an hour is restarted at once every hour.
const HEALTHY: Duration = Duration::from_secs(60);

/// What this process was told about itself before the environment was scrubbed.
static REMEMBERED: std::sync::OnceLock<Option<u32>> = std::sync::OnceLock::new();

/// Whether this process is the child, which serves.
///
/// # Invariant
///
/// The variable is not proof: a shell inherits the environment, so a server
/// started from one would read its grandparent's supervisor and skip a watchdog
/// of its own. The claim is checked against the process the OS names as this
/// one's parent, which a shell cannot fake.
pub fn is_child() -> bool {
    let Some(claimed) = parse_supervisor() else {
        return false;
    };
    match crate::process::parent_of(std::process::id()) {
        Some(parent) => parent == claimed,
        // Unverified: an extra supervisor is harmless, its child verifies.
        None => false,
    }
}

/// The supervisor's pid, when this process was started by one.
///
/// # Invariant
///
/// Read once, because [`remember`] takes the variables out of this process's
/// environment. A greeting built later must still answer what this process was.
pub fn supervisor() -> Option<u32> {
    match REMEMBERED.get() {
        Some(pid) => *pid,
        None => parse_supervisor(),
    }
}

/// The exit an earlier server left behind, as `<code>@<unix millis>`.
pub fn previous_exit() -> Option<String> {
    std::env::var(LAST_EXIT)
        .ok()
        .filter(|s| !s.trim().is_empty())
}

/// Takes what the supervisor said about this process, and stops passing it on.
///
/// # Invariant
///
/// These three variables describe *this* process, not the ones it starts. A shell
/// a session runs inherits the environment, so leaving them set made every server
/// started from an agent's shell believe it was already a child -- no watchdog of
/// its own, and a greeting naming the outer server's supervisor. Nothing this
/// process starts may read them, and no child of a child either.
pub fn remember() -> Option<u32> {
    let pid = parse_supervisor();
    let _ = REMEMBERED.set(pid);
    // SAFETY: called before anything reads the environment on another thread.
    unsafe {
        for name in [SUPERVISED, SUPERVISOR, LAST_EXIT] {
            std::env::remove_var(name);
        }
    }
    pid
}

fn parse_supervisor() -> Option<u32> {
    std::env::var(SUPERVISOR).ok()?.trim().parse().ok()
}

/// What the supervisor does with the child that just ended.
#[derive(Debug, PartialEq, Eq)]
enum Next {
    /// Leave with this code: the child was asked to stop, or said why it could not run.
    Leave(i32),
    /// Start it again, and say why in the log.
    Restart(String),
}

/// Compares a child's exit with the code contract.
///
/// # Invariant
///
/// Asked-to-stop and refuse-to-start are answers, so neither is retried: a
/// restart of a server whose home is held by a live process would loop forever
/// on a fact nothing changes. A process that died without saying anything -- a
/// panic, a kill, a signal -- is the crash this exists for.
fn next(status: &ExitStatus) -> Next {
    match status.code() {
        Some(0) => Next::Leave(0),
        Some(INSISTED) => Next::Leave(INSISTED),
        Some(REFUSED) => Next::Leave(REFUSED),
        Some(code) => Next::Restart(format!("exit code {code}")),
        None => Next::Restart("a signal".to_string()),
    }
}

/// Runs the server under a supervisor, and answers the code to leave with.
///
/// # Invariant
///
/// The child is this same program with this same argv, so `--parent` still names
/// the window: the stop it asks for is the one the server already knows. Anything
/// this process knew and the child must not inherit -- that it is the supervisor --
/// travels in the environment, which a successor written by `evolve` does not copy
/// from its predecessor's argv.
pub fn watch() -> i32 {
    ignore_interrupts();
    let program = match std::env::current_exe() {
        Ok(program) => program,
        Err(e) => {
            eprintln!("watchdog: cannot resolve this program: {e}");
            return 1;
        }
    };
    let args: Vec<OsString> = std::env::args_os().skip(1).collect();
    let mut backoff = MIN_BACKOFF;
    let mut crash: Option<String> = None;
    loop {
        let started = Instant::now();
        let mut cmd = Command::new(&program);
        cmd.args(&args)
            .env(SUPERVISED, "1")
            .env(SUPERVISOR, std::process::id().to_string());
        if let Some(crash) = crash.take() {
            cmd.env(LAST_EXIT, crash);
        }
        let mut child = match cmd.spawn() {
            Ok(child) => child,
            Err(e) => {
                eprintln!("watchdog: cannot start {}: {e}", program.display());
                return 1;
            }
        };
        let status = match child.wait() {
            Ok(status) => status,
            Err(e) => {
                eprintln!("watchdog: cannot wait for the server: {e}");
                return 1;
            }
        };
        match next(&status) {
            Next::Leave(code) => return code,
            Next::Restart(why) => {
                if started.elapsed() >= HEALTHY {
                    backoff = MIN_BACKOFF;
                }
                eprintln!(
                    "watchdog: the server stopped without saying why ({why}); \
                     restarting in {} ms",
                    backoff.as_millis()
                );
                crash = Some(format!(
                    "{}@{}",
                    status.code().unwrap_or(-1),
                    crate::store::now_millis()
                ));
                std::thread::sleep(backoff);
                backoff = (backoff * 2).min(MAX_BACKOFF);
            }
        }
    }
}

/// Whether this argv asks a question that a supervisor has no part in.
///
/// # Invariant
///
/// `--version` and `--help` answer without a home, a config or a child. A
/// supervisor in front of them would build a process to print one line and die.
pub fn answers_alone() -> bool {
    std::env::args_os()
        .any(|arg| arg == "--help" || arg == "-h" || arg == "--version" || arg == "-V")
}

/// Keeps a console's Ctrl-C from reaching this process: the child answers it.
///
/// # Invariant
///
/// A supervisor that dies on the first Ctrl-C leaves its child running with no
/// one to restart it, which is the one state this process exists to avoid.
fn ignore_interrupts() {
    #[cfg(windows)]
    {
        use windows_sys::Win32::System::Console::{
            CTRL_BREAK_EVENT, CTRL_C_EVENT, SetConsoleCtrlHandler,
        };

        unsafe extern "system" fn ignore(event: u32) -> i32 {
            i32::from(matches!(event, CTRL_C_EVENT | CTRL_BREAK_EVENT))
        }
        // SAFETY: `ignore` only reads its argument.
        unsafe {
            SetConsoleCtrlHandler(Some(ignore), 1);
        }
    }
    #[cfg(unix)]
    {
        // SAFETY: ignoring a signal has no preconditions and no memory effects.
        unsafe {
            libc::signal(libc::SIGINT, libc::SIG_IGN);
            libc::signal(libc::SIGTERM, libc::SIG_IGN);
        }
    }
}
