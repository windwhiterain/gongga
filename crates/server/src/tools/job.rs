//! One job object per persistent shell, on Windows.
//!
//! Windows has no process group, so ending "a shell and everything it started"
//! has to be built out of something. `taskkill /T` looks like that thing and is
//! not: it walks the tree from the root down, and a process forked between the
//! sweep and the root's death is left behind as an orphan that still holds the
//! shell's pipes. That orphan is not cosmetic -- the runtime reads those pipes
//! on blocking threads, and a read does not return until the last writer
//! closes, so the server cannot finish shutting down while one survives.
//!
//! A job object is the primitive that actually means the whole tree. Every
//! process the shell starts inherits membership, and `KILL_ON_JOB_CLOSE` makes
//! closing the handle terminate all of them, so there is no tree to walk and no
//! window to miss.
//!
//! Only Windows needs this. On unix the shell is spawned into its own process
//! group and `kill -TERM -<pid>` already means the tree.

use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};
use windows_sys::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation,
    SetInformationJobObject, TerminateJobObject,
};
use windows_sys::Win32::System::Threading::{OpenProcess, PROCESS_SET_QUOTA, PROCESS_TERMINATE};

/// A job object with one shell in it.
///
/// Dropping it terminates the shell and every process the shell started, which
/// is the behaviour we want in every case: a shell is either in use, or being
/// thrown away.
pub struct ProcessJob {
    handle: HANDLE,
}

// The handle is a plain kernel handle: no thread affinity, no aliasing rules
// beyond "close it exactly once", which the `Drop` below guarantees.
unsafe impl Send for ProcessJob {}
unsafe impl Sync for ProcessJob {}

impl ProcessJob {
    /// Creates an empty job and puts `pid` in it.
    ///
    /// `None` means the job could not be created or the process could not be
    /// adopted -- the caller falls back to killing the pid, which is what it
    /// did before this existed.
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
        // `KILL_ON_JOB_CLOSE` is what makes this a cleanup rather than a leak:
        // closing the last handle to the job ends everything in it.
        unsafe {
            CloseHandle(self.handle);
        }
    }
}
