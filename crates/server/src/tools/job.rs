//! One job object per persistent shell, on Windows.
//!
//! See `docs/notes/server-tools-job.md`.

use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};
use windows_sys::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, TH32CS_SNAPTHREAD, THREADENTRY32, Thread32First, Thread32Next,
};
use windows_sys::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    JOBOBJECT_BASIC_ACCOUNTING_INFORMATION, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
    JobObjectBasicAccountingInformation, JobObjectExtendedLimitInformation,
    QueryInformationJobObject, SetInformationJobObject, TerminateJobObject,
};
use windows_sys::Win32::System::Threading::{
    OpenProcess, OpenThread, PROCESS_SET_QUOTA, PROCESS_TERMINATE, ResumeThread,
    THREAD_SUSPEND_RESUME,
};

/// A job object with one shell in it; dropping it ends the shell and its processes.
pub struct ProcessJob {
    handle: HANDLE,
}

// A plain kernel handle: no thread affinity, no aliasing beyond closing it once.
unsafe impl Send for ProcessJob {}
unsafe impl Sync for ProcessJob {}

impl ProcessJob {
    /// Creates an empty job and puts `pid` in it; `None` means the caller kills the pid.
    pub fn adopt(pid: u32) -> Option<Self> {
        unsafe {
            let handle = CreateJobObjectW(std::ptr::null(), std::ptr::null());
            if handle.is_null() {
                return None;
            }

            let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
            info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            let sized = SetInformationJobObject(
                handle,
                JobObjectExtendedLimitInformation,
                &info as *const _ as *const core::ffi::c_void,
                std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            );
            if sized == 0 {
                CloseHandle(handle);
                return None;
            }

            // The process handle is only needed to join the job; membership is
            // what outlives it.
            let process = OpenProcess(PROCESS_SET_QUOTA | PROCESS_TERMINATE, 0, pid);
            if process.is_null() {
                CloseHandle(handle);
                return None;
            }
            let joined = AssignProcessToJobObject(handle, process);
            CloseHandle(process);
            if joined == 0 {
                CloseHandle(handle);
                return None;
            }
            Some(Self { handle })
        }
    }

    /// Resumes the only thread a process created suspended can have.
    ///
    /// # Invariant
    ///
    /// Called only after the process joined its job. Before this, no shell input
    /// or descendant can escape the job; afterwards, every child belongs to it.
    pub fn resume_main(pid: u32) -> std::io::Result<()> {
        unsafe {
            let snapshot = CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0);
            if snapshot.is_null() {
                return Err(std::io::Error::last_os_error());
            }
            let mut entry: THREADENTRY32 = std::mem::zeroed();
            entry.dwSize = std::mem::size_of::<THREADENTRY32>() as u32;
            let mut found = Thread32First(snapshot, &mut entry) != 0;
            let mut thread = None;
            while found {
                if entry.th32OwnerProcessID == pid {
                    if thread.is_some() {
                        CloseHandle(snapshot);
                        return Err(std::io::Error::other(
                            "a suspended process has more than one initial thread",
                        ));
                    }
                    thread = Some(entry.th32ThreadID);
                }
                found = Thread32Next(snapshot, &mut entry) != 0;
            }
            CloseHandle(snapshot);
            let id = thread.ok_or_else(|| {
                std::io::Error::other("a suspended process has no initial thread")
            })?;
            let thread = OpenThread(THREAD_SUSPEND_RESUME, 0, id);
            if thread.is_null() {
                return Err(std::io::Error::last_os_error());
            }
            let resumed = ResumeThread(thread);
            CloseHandle(thread);
            if resumed == u32::MAX {
                Err(std::io::Error::last_os_error())
            } else {
                Ok(())
            }
        }
    }

    /// Ends every process in the job. Idempotent; `Drop` does it too.
    pub fn terminate(&self) {
        unsafe {
            TerminateJobObject(self.handle, 1);
        }
    }

    /// How many members are alive, which is the only proof a shell left nothing.
    ///
    /// # Invariant
    ///
    /// A count of zero is what "the whole tree is gone" means here: a member that
    /// left the job is not in it, so a survivor is always counted. See
    /// `docs/notes/server-remote.md`.
    pub fn active_processes(&self) -> Option<u32> {
        let mut info: JOBOBJECT_BASIC_ACCOUNTING_INFORMATION = unsafe { std::mem::zeroed() };
        let mut returned = 0u32;
        let asked = unsafe {
            QueryInformationJobObject(
                self.handle,
                JobObjectBasicAccountingInformation,
                &mut info as *mut _ as *mut core::ffi::c_void,
                std::mem::size_of::<JOBOBJECT_BASIC_ACCOUNTING_INFORMATION>() as u32,
                &mut returned,
            )
        };
        (asked != 0).then_some(info.ActiveProcesses)
    }
}

impl Drop for ProcessJob {
    fn drop(&mut self) {
        // `KILL_ON_JOB_CLOSE` makes closing the last handle the cleanup, not a leak.
        unsafe {
            CloseHandle(self.handle);
        }
    }
}
