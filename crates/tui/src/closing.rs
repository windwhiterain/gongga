//! The console going away under the window, which only the platform can say.
//!
//! See `docs/notes/tui-closing.md`.

use std::future::pending;
use std::sync::OnceLock;

use tokio::sync::Notify;

/// The one notifier the platform's thread and the loop share.
static WAKE: OnceLock<Notify> = OnceLock::new();

/// Asks this window to leave when the console under it goes.
///
/// # Invariant
///
/// The ask reaches the loop, and the loop leaves the way Alt-Q leaves: a closing
/// console is not a terminal to give back, and nothing here writes to it. See
/// `docs/notes/tui-closing.md`.
///
/// # Invariant
///
/// Only the asks that mean the console is going away are taken. Ctrl-C is this
/// window's own key and stays one: `CTRL_C_EVENT` and `CTRL_BREAK_EVENT` return
/// the default action, which is what keeps a stopped window interruptible.
pub fn watch() -> bool {
    let _ = WAKE.set(Notify::new());
    install()
}

/// Resolves once the console has asked this window to close.
///
/// # Invariant
///
/// The platform delivers this ask only after the console's output pipe has been
/// drained, so a host that stops reading leaves it unresolved for good and no
/// handler can reach the window. See `docs/notes/tui-closing.md`.
pub async fn asked() {
    match WAKE.get() {
        Some(wake) => wake.notified().await,
        // Asked before the handler exists: nobody is going to arrive.
        None => pending().await,
    }
}

/// Installs the platform's handler, and says whether a console that goes away
/// will be seen.
#[cfg(windows)]
fn install() -> bool {
    use windows_sys::Win32::System::Console::SetConsoleCtrlHandler;

    // SAFETY: `handle` is a plain function and touches only this module's statics.
    unsafe { SetConsoleCtrlHandler(Some(handle), 1) != 0 }
}

/// The platform's ask, on a thread of its own.
#[cfg(windows)]
unsafe extern "system" fn handle(event: u32) -> i32 {
    use windows_sys::Win32::System::Console::{
        CTRL_CLOSE_EVENT, CTRL_LOGOFF_EVENT, CTRL_SHUTDOWN_EVENT,
    };

    if !matches!(
        event,
        CTRL_CLOSE_EVENT | CTRL_LOGOFF_EVENT | CTRL_SHUTDOWN_EVENT
    ) {
        return 0;
    }
    // Nothing is written and nothing waits: this thread belongs to the platform,
    // and the loop is the one that owns the way out.
    if let Some(wake) = WAKE.get() {
        wake.notify_one();
    }
    1
}

/// A closed console already ends this process on the platforms without one.
#[cfg(not(windows))]
fn install() -> bool {
    // A console that closes sends `SIGHUP`, whose default action is to end this
    // process: there is nothing to install, and nothing missing.
    true
}
