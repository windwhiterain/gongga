//! A Ctrl-C, a closed window, a SIGTERM: asked for, and answered as `cancel`.
//!
//! See `docs/notes/server-signal.md`.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use tokio::sync::Notify;

/// The asks so far: the first cancels, a second one leaves without waiting.
static ASKS: AtomicUsize = AtomicUsize::new(0);

/// The one notifier every ask reaches, and the one the followed process reaches
/// too.
static WAKE: std::sync::OnceLock<Arc<Notify>> = std::sync::OnceLock::new();

/// The pid this server follows, once it follows one; 0 is nobody.
static OWNER: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

/// Which ask reached this process first, in words a log row can keep.
///
/// # Invariant
///
/// The first ask is the one that stops the server, so the first one to set this
/// is the truth. A second ask leaves at once and never reaches here. Recorded
/// because "the server stopped" leaves a reader asking who asked, and a signal
/// has no client to name. See `docs/notes/server-signal.md`.
static REASON: std::sync::OnceLock<String> = std::sync::OnceLock::new();

/// What asked this server to stop, once something did.
pub fn reason() -> Option<&'static str> {
    REASON.get().map(String::as_str)
}

/// The pid this server follows: the greeting carries it, so a client can say
/// whether closing it would stop this server.
pub fn owner() -> Option<u32> {
    match OWNER.load(Ordering::SeqCst) {
        0 => None,
        pid => Some(pid),
    }
}

/// Runs `stop` once, when the operating system asks this process to stop.
///
/// # Invariant
/// The handler never stops anything itself: it only nudges. The stop runs on the
/// runtime, on the path `cancel` already takes -- every running turn is
/// interrupted, and a signal's stop ends its background jobs rather than letting
/// them go.
pub fn watch<F, Fut>(parent: Option<u32>, parent_build: Option<&str>, stop: F)
where
    F: FnOnce() -> Fut + Send + 'static,
    Fut: std::future::Future<Output = ()> + Send + 'static,
{
    let wake = Arc::new(Notify::new());
    // Before anything can connect: the handler and the followed process nudge one
    // notifier, whichever of them arrives first.
    let _ = WAKE.set(wake.clone());
    if !install(wake.clone()) {
        tracing::warn!("no signal handler: a Ctrl-C will not interrupt the turns");
    }
    if let Some(pid) = parent {
        follow_pid(pid, parent_build);
    }
    waiter(wake, stop);
}

/// The whole of the reaction, so what a handler reaches is one call.
fn waiter<F, Fut>(wake: Arc<Notify>, stop: F) -> tokio::task::JoinHandle<()>
where
    F: FnOnce() -> Fut + Send + 'static,
    Fut: std::future::Future<Output = ()> + Send + 'static,
{
    tokio::spawn(async move {
        wake.notified().await;
        tracing::info!(
            reason = reason().unwrap_or("-"),
            "asked to stop from outside the protocol"
        );
        stop().await;
    })
}

/// What a handler may call: no lock, no block, and the reason kept with the nudge.
fn nudge(reason: &str, wake: &Notify) {
    let _ = REASON.set(reason.to_string());
    wake.notify_one();
}

/// Follows `pid`, the process this server was started for.
///
/// # Invariant
///
/// `--parent` is ownership, decided before any connection: a dial reads and never
/// owns, so a server started by hand outlives the windows that read it, and a
/// handover carries the flag over. A window of another build is not followed --
/// it cannot read this one, and its death must not take the successor with it.
/// See `docs/notes/server-signal.md`.
fn follow_pid(pid: u32, build: Option<&str>) {
    if let Some(window) = build {
        if window != protocol::BUILD {
            tracing::info!(
                pid,
                window = window,
                this = protocol::BUILD,
                "the window this server was started for runs another build; not following it"
            );
            return;
        }
    }
    OWNER.store(pid, Ordering::SeqCst);
    if let Some(wake) = WAKE.get() {
        follows(pid, wake.clone());
    }
}

/// A signal, asked once or insisted on.
///
/// # Invariant
/// The first ask interrupts every turn and writes the `server_stop` row; a
/// second ask leaves at once, so a turn that ignores the token cannot own the
/// console.
fn asked(reason: &str, wake: &Notify) {
    if ASKS.fetch_add(1, Ordering::SeqCst) == 0 {
        nudge(reason, wake);
    } else {
        std::process::exit(130);
    }
}

/// Follows the process that started this server, when one is named.
///
/// # Invariant
///
/// The parent is the window that owns this server, so its death is a stop
/// request, not a kill: it takes the same path as a signal. The wait holds no
/// work, so the exit never waits for the window. In the blocking pool, which
/// `Runtime::drop` joins, it was a stop held for the window's lifetime.
fn follows(pid: u32, wake: Arc<Notify>) {
    tokio::spawn(async move {
        while exists(pid) {
            tokio::time::sleep(POLL).await;
        }
        nudge(
            &format!("the process this server follows (pid {pid}) ended"),
            &wake,
        );
    });
}

/// How often a followed pid is asked whether it is still there.
const POLL: std::time::Duration = std::time::Duration::from_millis(250);

/// Whether a pid names a live process.
#[cfg(unix)]
fn exists(pid: u32) -> bool {
    // SAFETY: signal 0 delivers nothing; it only asks whether the pid is there.
    let refused = unsafe { libc::kill(pid as libc::pid_t, 0) } != 0;
    let error = std::io::Error::last_os_error().raw_os_error();
    // Somebody else's process is still somebody there.
    !refused || error == Some(libc::EPERM)
}

/// Whether a pid names a live process, by its exit code.
///
/// # Invariant
///
/// Asking, not waiting: a timed wait would hold a runtime thread for its length,
/// and an untimed one is the defect above.
#[cfg(windows)]
fn exists(pid: u32) -> bool {
    use windows_sys::Win32::Foundation::{CloseHandle, STILL_ACTIVE};
    use windows_sys::Win32::System::Threading::{
        GetExitCodeProcess, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
    };
    if pid == 0 {
        return false;
    }
    // SAFETY: a query handle on a pid that may already be gone is answered with
    // null, and the handle is closed exactly once.
    unsafe {
        let process = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if process.is_null() {
            return false;
        }
        let mut code = 0;
        let alive = GetExitCodeProcess(process, &mut code) != 0 && code == STILL_ACTIVE as u32;
        CloseHandle(process);
        alive
    }
}

#[cfg(not(any(unix, windows)))]
fn exists(_pid: u32) -> bool {
    false
}

#[cfg(windows)]
fn install(_wake: Arc<Notify>) -> bool {
    use windows_sys::Win32::System::Console::SetConsoleCtrlHandler;

    // SAFETY: `handle` is a plain function, and it only touches a `OnceLock`.
    unsafe { SetConsoleCtrlHandler(Some(handle), 1) != 0 }
}

#[cfg(windows)]
unsafe extern "system" fn handle(event: u32) -> i32 {
    use windows_sys::Win32::System::Console::{
        CTRL_BREAK_EVENT, CTRL_C_EVENT, CTRL_CLOSE_EVENT, CTRL_LOGOFF_EVENT, CTRL_SHUTDOWN_EVENT,
    };

    let stopping = matches!(
        event,
        CTRL_C_EVENT
            | CTRL_BREAK_EVENT
            | CTRL_CLOSE_EVENT
            | CTRL_LOGOFF_EVENT
            | CTRL_SHUTDOWN_EVENT
    );
    if stopping {
        if let Some(wake) = WAKE.get() {
            asked(&console_ask(event), wake);
        }
    }
    // Anything else keeps the default action.
    if stopping { 1 } else { 0 }
}

/// Which console event asked: the words a reader of the record can act on.
#[cfg(windows)]
fn console_ask(event: u32) -> String {
    use windows_sys::Win32::System::Console::{
        CTRL_BREAK_EVENT, CTRL_C_EVENT, CTRL_CLOSE_EVENT, CTRL_LOGOFF_EVENT, CTRL_SHUTDOWN_EVENT,
    };
    let words = match event {
        CTRL_C_EVENT => "Ctrl-C in its console",
        CTRL_BREAK_EVENT => "Ctrl-Break in its console",
        CTRL_CLOSE_EVENT => "its console closing",
        CTRL_LOGOFF_EVENT => "a logoff",
        CTRL_SHUTDOWN_EVENT => "a shutdown",
        _ => "a console event",
    };
    format!("{words} (a signal to the server)")
}

#[cfg(unix)]
fn install(wake: Arc<Notify>) -> bool {
    use tokio::signal::unix::{SignalKind, signal};

    let signals = (
        signal(SignalKind::interrupt()),
        signal(SignalKind::terminate()),
    );
    let (interrupt, terminate) = match signals {
        (Ok(interrupt), Ok(terminate)) => (interrupt, terminate),
        _ => return false,
    };
    tokio::spawn(async move {
        let (mut interrupt, mut terminate) = (interrupt, terminate);
        loop {
            let asked_by = tokio::select! {
                _ = interrupt.recv() => "SIGINT (a signal to the server)",
                _ = terminate.recv() => "SIGTERM (a signal to the server)",
            };
            asked(asked_by, &wake);
        }
    });
    true
}
