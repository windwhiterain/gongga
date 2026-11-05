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
