//! One job object per persistent shell, on Windows.
//!
//! See `docs/notes/server-tools-job.md`.

use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};
use windows_sys::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation,
    SetInformationJobObject, TerminateJobObject,
};
use windows_sys::Win32::System::Threading::{OpenProcess, PROCESS_SET_QUOTA, PROCESS_TERMINATE};

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

    /// Ends every process in the job. Idempotent; `Drop` does it too.
    pub fn terminate(&self) {
        unsafe {
            TerminateJobObject(self.handle, 1);
        }
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
