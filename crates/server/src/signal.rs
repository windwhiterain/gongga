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
pub fn watch<F, Fut>(stop: F)
where
    F: FnOnce() -> Fut + Send + 'static,
    Fut: std::future::Future<Output = ()> + Send + 'static,
{
    let wake = Arc::new(Notify::new());
    if !install(wake.clone()) {
        tracing::warn!("no signal handler: a Ctrl-C will not preserve the turn");
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
        tracing::info!("asked to stop by a signal");
        stop().await;
    })
}

/// What a handler may call: no lock, no block, and a nudge is remembered.
///
/// # Invariant
/// The first ask cancels and writes the pending row; a second ask leaves at
/// once, so a turn that ignores the cancel cannot own the console.
fn nudge(wake: &Notify) {
    if ASKS.fetch_add(1, Ordering::SeqCst) == 0 {
        wake.notify_one();
    } else {
        std::process::exit(130);
    }
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
            nudge(wake);
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
            nudge(&wake);
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
}
