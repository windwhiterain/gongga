//! Job objects: what "this process and everything it started" means.
//!
//! See `docs/notes/server-tools-job.md`.

use std::collections::HashMap;
use std::sync::LazyLock;
use std::sync::Mutex;
use std::sync::MutexGuard;

pub use relay::job::ProcessJob;

/// How a tree this server holds ends with it.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Ends {
    /// A `cancel` lets it go: a background job, which was started to outlive the server.
    OnCancel,
    /// Nothing lets it go: a call the server is running, which no stop spares.
    WithServer,
}

struct Armed {
    job: ProcessJob,
    ends: Ends,
}

/// Every tree this server holds, by the pid it was armed for.
///
/// # Invariant
///
/// One registry: exactly one act lets a process outlive this server -- `cancel` -- and
/// it must reach every kind of background job. No owner may hold a handle itself, or a
/// handler that dropped would be the thing deciding.
static ARMED: LazyLock<Mutex<HashMap<u32, Armed>>> = LazyLock::new(|| Mutex::new(HashMap::new()));

fn armed() -> MutexGuard<'static, HashMap<u32, Armed>> {
    ARMED.lock().unwrap_or_else(|e| e.into_inner())
}

/// Puts `pid`'s tree under this server's life: only a `cancel` lets it go.
pub fn arm(pid: u32) -> bool {
    hold(pid, Ends::OnCancel)
}

/// The same, with nothing that lets it go: a call the server is answering.
pub fn hold_served(pid: u32) -> bool {
    hold(pid, Ends::WithServer)
}

fn hold(pid: u32, ends: Ends) -> bool {
    if pid == 0 {
        return false;
    }
    let Some(job) = ProcessJob::adopt_closed(pid) else {
        return false;
    };
    let replaced = armed().insert(pid, Armed { job, ends });
    if let Some(replaced) = replaced {
        // One tree, two handles: the older is let go, not closed, or arming kills it.
        replaced.job.release();
    }
    true
}

/// Lets `pid`'s tree outlive this server, now and after it is gone.
pub fn release(pid: u32) {
    if let Some(held) = armed().remove(&pid) {
        held.job.release();
    }
}

/// Lets every background tree go, and says how many there were.
///
/// # Invariant
///
/// The whole of what `cancel` means for a background job. Every other stop calls
/// `terminate_all` instead, which ends the same trees where they stand.
pub fn release_all() -> usize {
    let mut registry = armed();
    let going: Vec<u32> = registry
        .iter()
        .filter(|(_, held)| held.ends == Ends::OnCancel)
        .map(|(pid, _)| *pid)
        .collect();
    let count = going.len();
    for pid in going {
        if let Some(held) = registry.remove(&pid) {
            held.job.release();
        }
    }
    count
}

/// Ends every background tree this server holds, and says how many there were.
///
/// # Invariant
///
/// The counterpart of `release_all`: what a stop that is not a `cancel` means for
/// a background job. `WithServer` trees are left to their own callers. The job
/// objects would take their trees with the process anyway; doing it here makes the
/// kill an act of the stop rather than a side effect of exit.
pub fn terminate_all() -> usize {
    let mut registry = armed();
    let going: Vec<u32> = registry
        .iter()
        .filter(|(_, held)| held.ends == Ends::OnCancel)
        .map(|(pid, _)| *pid)
        .collect();
    let count = going.len();
    for pid in going {
        if let Some(held) = registry.remove(&pid) {
            held.job.terminate();
        }
    }
    count
}

/// Ends `pid`'s tree now, and forgets it: what a caller's stop means.
pub fn disarm(pid: u32) -> bool {
    match armed().remove(&pid) {
        Some(held) => {
            held.job.terminate();
            drop(held);
            true
        }
        None => false,
    }
}
