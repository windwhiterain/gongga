//! A Ctrl-C, a closed window, a SIGTERM: asked for, and answered as `cancel`.
//!
//! See `docs/notes/server-signal.md`.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use tokio::sync::Notify;

/// The asks so far: the first cancels, a second one leaves without waiting.
static ASKS: AtomicUsize = AtomicUsize::new(0);

/// Runs `stop` once, when the operating system asks this process to stop.
///
/// # Invariant
/// The handler never stops anything itself: it only nudges. The stop runs on the
/// runtime, on the path `cancel` already takes -- the running turn is preserved
/// as pending, and the next server resumes it.
pub fn watch<F, Fut>(parent: Option<u32>, stop: F)
where
    F: FnOnce() -> Fut + Send + 'static,
    Fut: std::future::Future<Output = ()> + Send + 'static,
{
    let wake = Arc::new(Notify::new());
    if !install(wake.clone()) {
        tracing::warn!("no signal handler: a Ctrl-C will not preserve the turn");
    }
    if let Some(pid) = parent {
        follows(pid, wake.clone());
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
        tracing::info!("asked to stop from outside the protocol");
        stop().await;
    })
}

/// What a handler may call: no lock, no block, and a nudge is remembered.
fn nudge(wake: &Notify) {
    wake.notify_one();
}

/// A signal, asked once or insisted on.
///
/// # Invariant
/// The first ask cancels and writes the pending row; a second ask leaves at
/// once, so a turn that ignores the cancel cannot own the console.
fn asked(wake: &Notify) {
    if ASKS.fetch_add(1, Ordering::SeqCst) == 0 {
        nudge(wake);
    } else {
        std::process::exit(130);
    }
}

/// Follows the process that started this server, when one is named.
///
/// # Invariant
///
/// A named parent is the window that owns this server, so its death is a stop
/// request -- not a kill: the turn in flight is preserved like any other stop.
#[cfg(windows)]
fn follows(pid: u32, wake: Arc<Notify>) {
    use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};
    use windows_sys::Win32::System::Threading::{
        INFINITE, OpenProcess, PROCESS_SYNCHRONIZE, WaitForSingleObject,
    };

    // SAFETY: asking for a wait handle on a pid that may already be gone.
    let handle = unsafe { OpenProcess(PROCESS_SYNCHRONIZE, 0, pid) };
    if handle.is_null() {
        nudge(&wake);
        return;
    }
    // As a number: a handle is not `Send`, and this thread only waits on it.
    let raw = handle as usize;
    // A thread of its own: the handle signals only when the window ends.
    tokio::task::spawn_blocking(move || {
        let handle = raw as HANDLE;
        // SAFETY: the handle came from `OpenProcess`, and it is closed once.
        unsafe {
            WaitForSingleObject(handle, INFINITE);
            CloseHandle(handle);
        }
        nudge(&wake);
    });
}

/// The same on unix, by asking the kernel whether the pid still exists.
#[cfg(unix)]
fn follows(pid: u32, wake: Arc<Notify>) {
    tokio::spawn(async move {
        while exists(pid) {
            tokio::time::sleep(std::time::Duration::from_millis(250)).await;
        }
        nudge(&wake);
    });
}

#[cfg(unix)]
fn exists(pid: u32) -> bool {
    // SAFETY: signal 0 delivers nothing; it only asks whether the pid is there.
    let refused = unsafe { libc::kill(pid as libc::pid_t, 0) } != 0;
    let error = std::io::Error::last_os_error().raw_os_error();
    // Somebody else's process is still somebody there.
    !refused || error == Some(libc::EPERM)
}

#[cfg(windows)]
static WAKE: std::sync::OnceLock<Arc<Notify>> = std::sync::OnceLock::new();

#[cfg(windows)]
fn install(wake: Arc<Notify>) -> bool {
    use windows_sys::Win32::System::Console::SetConsoleCtrlHandler;

    if WAKE.set(wake).is_err() {
        return false;
    }
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
            asked(wake);
        }
    }
    // Anything else keeps the default action.
    if stopping { 1 } else { 0 }
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
            tokio::select! {
                _ = interrupt.recv() => {}
                _ = terminate.recv() => {}
            }
            asked(&wake);
        }
    });
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// One nudge, one stop -- even when the nudge lands before the waiter polls.
    #[tokio::test]
    async fn a_nudge_reaches_the_stop_once() {
        let woke = Arc::new(AtomicUsize::new(0));
        let counted = woke.clone();
        let wake = Arc::new(Notify::new());

        let stop = waiter(wake.clone(), move || async move {
            counted.fetch_add(1, Ordering::SeqCst);
        });
        // Before the waiter polls, which is what a console handler does.
        nudge(&wake);
        stop.await.unwrap();

        assert_eq!(woke.load(Ordering::SeqCst), 1);
    }

    /// A window that is already gone is a stop asked for at startup.
    #[cfg(windows)]
    #[tokio::test]
    async fn a_parent_already_gone_asks_to_stop() {
        let woke = Arc::new(AtomicUsize::new(0));
        let counted = woke.clone();
        let wake = Arc::new(Notify::new());
        let stop = waiter(wake.clone(), move || async move {
            counted.fetch_add(1, Ordering::SeqCst);
        });
        // No process has this pid, and none will: `u32::MAX` is not one.
        follows(u32::MAX, wake);
        tokio::time::timeout(std::time::Duration::from_secs(10), stop)
            .await
            .expect("a gone parent must ask at once")
            .unwrap();
        assert_eq!(woke.load(Ordering::SeqCst), 1);
    }
}
