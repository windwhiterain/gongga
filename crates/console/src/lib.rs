//! How a program the harness starts keeps its console off the user's desktop.
//!
//! See `docs/notes/console-lib.md`.

/// The flag: a console of the program's own, with no window on the desktop.
#[cfg(windows)]
pub const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// The flag's value on the platforms that have no console to give.
#[cfg(not(windows))]
pub const CREATE_NO_WINDOW: u32 = 0;

/// The creation flags a background program carries: `extra` and the windowless console.
#[cfg(windows)]
pub fn flags(extra: u32) -> u32 {
    extra | CREATE_NO_WINDOW
}

/// The creation flags a background program the harness starts carries.
#[cfg(not(windows))]
pub fn flags(extra: u32) -> u32 {
    extra
}

/// Gives a command the windowless console, on the platforms that have one.
#[cfg(windows)]
pub fn apply(command: &mut std::process::Command, extra: u32) {
    use std::os::windows::process::CommandExt;
    command.creation_flags(flags(extra));
}

/// Gives a command the windowless console, on the platforms that have one.
#[cfg(not(windows))]
pub fn apply(_command: &mut std::process::Command, _extra: u32) {}

/// Keeps this process's own standard handles out of every program it starts.
///
/// # Invariant
///
/// The standard library asks Windows to hand the child every inheritable handle
/// this process holds, so a child as detached as a server still ends up with this
/// process's stdin and stdout. That is not a leak of resources, it is a leak of
/// the answer "the starter is gone": a window reads the client's stdout, and a
/// server holding the write end never lets the end arrive. See
/// `docs/notes/console-lib.md`, "The handles a child is not owed".
///
/// Sealed, not closed: this process keeps reading and writing them. Called before
/// the spawn rather than at startup, because only the program that starts a
/// separate one needs it. Unix needs nothing: a pipe there is close-on-exec.
#[cfg(windows)]
pub fn seal_std_handles() {
    use windows_sys::Win32::Foundation::{
        HANDLE, HANDLE_FLAG_INHERIT, INVALID_HANDLE_VALUE, SetHandleInformation,
    };
    use windows_sys::Win32::System::Console::{
        GetStdHandle, STD_ERROR_HANDLE, STD_INPUT_HANDLE, STD_OUTPUT_HANDLE,
    };
    for which in [STD_INPUT_HANDLE, STD_OUTPUT_HANDLE, STD_ERROR_HANDLE] {
        // A handle this process does not have is not a failure: a program started
        // without stdio has none, and nothing can be inherited from nothing.
        let handle: HANDLE = unsafe { GetStdHandle(which) };
        if handle.is_null() || handle == INVALID_HANDLE_VALUE {
            continue;
        }
        unsafe {
            SetHandleInformation(handle, HANDLE_FLAG_INHERIT, 0);
        }
    }
}

/// Keeps this process's standard handles out of its children.
#[cfg(not(windows))]
pub fn seal_std_handles() {}
