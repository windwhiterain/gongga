//! What the operating system says about the processes around this one.
//!
//! See `docs/notes/server-process.md`.

/// The process that started `pid`, as the OS sees it.
#[cfg(windows)]
pub fn parent_of(pid: u32) -> Option<u32> {
    use windows_sys::Win32::Foundation::{CloseHandle, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW,
        TH32CS_SNAPPROCESS,
    };

    // One snapshot per hop, not per walk: this runs on the startup path.
    unsafe {
        let snapshot = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
        if snapshot == INVALID_HANDLE_VALUE {
            return None;
        }
        let mut entry = PROCESSENTRY32W {
            dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32,
            ..Default::default()
        };
        let mut found = None;
        let mut more = Process32FirstW(snapshot, &mut entry) != 0;
        while more {
            if entry.th32ProcessID == pid {
                found = Some(entry.th32ParentProcessID);
                break;
            }
            more = Process32NextW(snapshot, &mut entry) != 0;
        }
        CloseHandle(snapshot);
        found
    }
}

/// The process that started `pid`, as the OS sees it.
#[cfg(unix)]
pub fn parent_of(pid: u32) -> Option<u32> {
    // One field, no header, and present wherever the shell is; `/proc` would be
    // faster but is not shaped the same on macOS.
    if pid == std::process::id() {
        // SAFETY: `getppid` reads one field of this process's own state.
        let pid = unsafe { libc::getppid() };
        return u32::try_from(pid).ok().filter(|pid| *pid != 0);
    }
    let out = std::process::Command::new("ps")
        .args(["-o", "ppid=", "-p", &pid.to_string()])
        .output()
        .ok()?;
    String::from_utf8_lossy(&out.stdout).trim().parse().ok()
}

#[cfg(not(any(windows, unix)))]
pub fn parent_of(_pid: u32) -> Option<u32> {
    None
}
