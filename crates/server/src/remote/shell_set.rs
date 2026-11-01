//! One session's persistent shells, wherever they run.
//!
//! See `docs/notes/server-remote.md`.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use tokio::sync::Mutex as AsyncMutex;

use std::sync::Mutex as StdMutex;

use crate::remote::endpoint::Endpoint;
use crate::remote::shell::{Door, RemoteShell};
use crate::remote::transport::{CloseReport, ShellControl};
use crate::tools::bash::{BashChunk, BashOutcome, BashShell};

/// One persistent shell: this machine's, or a peer's.
pub enum Shell {
    Local(BashShell),
    Remote(Box<RemoteShell>),
}

impl Shell {
    pub fn is_remote(&self) -> bool {
        matches!(self, Shell::Remote(_))
    }

    /// This machine's pid, for a shell that is this machine's: a peer's is not ours.
    pub fn local_pid(&self) -> Option<u32> {
        match self {
            Shell::Local(shell) => Some(shell.pid()),
            Shell::Remote(_) => None,
        }
    }

    pub fn cwd(&self) -> &Path {
        match self {
            Shell::Local(shell) => shell.cwd(),
            Shell::Remote(shell) => shell.cwd(),
        }
    }

    pub fn cwd_cell(&self) -> Arc<StdMutex<PathBuf>> {
        match self {
            Shell::Local(shell) => shell.cwd_cell(),
            Shell::Remote(shell) => shell.cwd_cell(),
        }
    }

    pub fn usable(&self) -> bool {
        match self {
            Shell::Local(shell) => shell.usable(),
            Shell::Remote(shell) => shell.usable(),
        }
    }

    pub fn pid(&self) -> u32 {
        match self {
            Shell::Local(shell) => shell.pid(),
            Shell::Remote(shell) => shell.pid(),
        }
    }

    pub fn pending_len(&self) -> usize {
        match self {
            Shell::Local(shell) => shell.pending_len(),
            Shell::Remote(shell) => shell.pending_len(),
        }
    }

    pub fn background_jobs(&self) -> usize {
        match self {
            Shell::Local(shell) => shell.background_jobs(),
            Shell::Remote(shell) => shell.background_jobs(),
        }
    }

    pub fn take_pending(&mut self) -> String {
        match self {
            Shell::Local(shell) => shell.take_pending(),
            Shell::Remote(shell) => shell.take_pending(),
        }
    }

    pub fn take_partial_output(&mut self) -> (String, String) {
        match self {
            Shell::Local(shell) => shell.take_partial_output(),
            Shell::Remote(shell) => shell.take_partial_output(),
        }
    }

    /// Runs one command, streaming to `sink`; the deadline covers waiting only.
    pub async fn run(
        &mut self,
        command: &str,
        timeout: std::time::Duration,
        sink: Option<&tokio::sync::mpsc::Sender<BashChunk>>,
    ) -> anyhow::Result<BashOutcome> {
        match self {
            Shell::Local(shell) => shell.run(command, timeout, sink).await,
            Shell::Remote(shell) => shell.run(command, timeout, sink).await,
        }
    }

    /// Reads what a command that outlived its deadline still had to say.
    pub async fn finish_outstanding(&mut self) -> (String, String, Option<i32>) {
        match self {
            Shell::Local(shell) => shell.finish_outstanding().await,
            Shell::Remote(shell) => shell.finish_outstanding().await,
        }
    }

    /// Starts a command without waiting.
    pub async fn run_background(
        &mut self,
        command: &str,
        notify: Option<crate::tools::bash::BackgroundNotifier>,
    ) -> anyhow::Result<u64> {
        match self {
            Shell::Local(shell) => shell.run_background(command, notify).await,
            Shell::Remote(shell) => shell.run_background(command, notify).await,
        }
    }

    /// Ends the shell and everything it started, reporting what could be proved.
    pub async fn kill(&mut self) -> CloseReport {
        match self {
            Shell::Local(shell) => {
                shell.kill().await;
                CloseReport {
                    confirmed: true,
                    unknown: None,
                }
            }
            Shell::Remote(shell) => shell.kill().await,
        }
    }
}

/// The persistent shells of one session, locked synchronously: spawn and remove.
pub struct ShellSet {
    /// Where new shells run: a program here, or an endpoint.
    endpoint: Endpoint,
    /// Where a new shell starts; `None` is the endpoint's own choice of directory.
    cwd: Option<PathBuf>,
    /// Where the default shell last said it was, mirrored out cell by cell.
    reported: Arc<StdMutex<PathBuf>>,
    /// Variables every local shell of this session starts with.
    env: Vec<(String, String)>,
    shells: HashMap<String, Arc<AsyncMutex<Shell>>>,
    /// Each endpoint shell's door: its end, decided outside its lock.
    doors: HashMap<String, Arc<Door>>,
    /// Each local shell's pid: a peer's pid names nothing on this machine.
    local_pids: HashMap<String, u32>,
    /// Commands handed to the background: out of `shells`, still this session's.
    retired: Vec<Discarded>,
}

impl ShellSet {
    pub fn new(endpoint: Endpoint, cwd: Option<PathBuf>, env: Vec<(String, String)>) -> Self {
        Self {
            endpoint,
            reported: Arc::new(StdMutex::new(cwd.clone().unwrap_or_default())),
            cwd,
            env,
            shells: HashMap::new(),
            doors: HashMap::new(),
            local_pids: HashMap::new(),
            retired: Vec::new(),
        }
    }

    /// Where this session's shells run.
    pub fn endpoint(&self) -> &Endpoint {
        &self.endpoint
    }

    /// Where the session's own shell last said it is; read without its lock.
    pub fn reported_cwd(&self) -> PathBuf {
        self.reported
            .lock()
            .map(|guard| guard.clone())
            .unwrap_or_default()
    }

    /// The shared handle for `name`, spawning it fresh on first use after a removal.
    ///
    /// # Invariant
    ///
    /// No network here: a remote shell is constructed unconnected, so a caller
    /// holding the session manager's live lock never waits on a peer.
    pub fn get(&mut self, name: &str) -> anyhow::Result<Arc<AsyncMutex<Shell>>> {
        if let Some(shell) = self.shells.get(name) {
            return Ok(shell.clone());
        }
        let shell = match &self.endpoint {
            Endpoint::Local { program } => {
                // This machine's directory: a local session always names one.
                let cwd = self
                    .cwd
                    .as_deref()
                    .ok_or_else(|| anyhow::anyhow!("a local shell needs a directory"))?;
                Shell::Local(BashShell::spawn(program, cwd, &self.env)?)
            }
            endpoint => {
                let door = Arc::new(Door::default());
                self.doors.insert(name.to_string(), door.clone());
                Shell::Remote(Box::new(RemoteShell::new(
                    endpoint.clone(),
                    self.cwd.clone(),
                    door,
                )))
            }
        };
        tracing::debug!(
            bash = %name,
            cwd = ?self.cwd,
            endpoint = %self.endpoint.describe(),
            "shell started"
        );
        // Only `default` is mirrored: a replacement brings a cell of its own.
        if name == crate::session::DEFAULT_BASH {
            self.reported = shell.cwd_cell();
        }
        if let Some(pid) = shell.local_pid() {
            self.local_pids.insert(name.to_string(), pid);
        }
        let handle = Arc::new(AsyncMutex::new(shell));
        self.shells.insert(name.to_string(), handle.clone());
        Ok(handle)
    }

    /// How `name` can be ended from outside its lock, once it has connected.
    pub fn control(&self, name: &str) -> Option<ShellControl> {
        self.doors.get(name)?.control()
    }

    /// The handle without spawning, for callers that must not create one.
    pub fn peek(&self, name: &str) -> Option<Arc<AsyncMutex<Shell>>> {
        self.shells.get(name).cloned()
    }

    /// The shells that exist right now, in a stable order.
    pub fn names(&self) -> Vec<String> {
        let mut names: Vec<String> = self.shells.keys().cloned().collect();
        names.sort();
        names
    }

    /// Takes `name` out of the set with the way to end whatever it left running.
    pub fn discard(&mut self, name: &str) -> Option<Discarded> {
        let handle = self.shells.remove(name)?;
        Some(Discarded {
            name: name.to_string(),
            handle,
            local_pid: self.local_pids.remove(name),
            door: self.doors.remove(name),
        })
    }

    /// Takes `handle` out of the set if it is still the one filed under `name`.
    pub fn retire(&mut self, name: &str, handle: &Arc<AsyncMutex<Shell>>) -> bool {
        let mine = self
            .shells
            .get(name)
            .map(|h| Arc::ptr_eq(h, handle))
            .unwrap_or(false);
        if mine {
            let discarded = self.discard(name);
            if let Some(discarded) = discarded {
                self.retired.push(discarded);
            }
        }
        mine
    }

    /// Retires `handle` and spawns a fresh shell in its place, keeping the name usable.
    pub fn replace(&mut self, name: &str, handle: &Arc<AsyncMutex<Shell>>) -> anyhow::Result<bool> {
        if !self.retire(name, handle) {
            return Ok(false);
        }
        self.get(name)?;
        Ok(true)
    }

    /// Takes the active shell and every retired shell that ever carried `name`.
    pub fn discard_all(&mut self, name: &str) -> Vec<Discarded> {
        let mut discarded = Vec::new();
        if let Some(shell) = self.discard(name) {
            discarded.push(shell);
        }
        let mut index = 0;
        while index < self.retired.len() {
            if self.retired[index].name == name {
                discarded.push(self.retired.remove(index));
            } else {
                index += 1;
            }
        }
        discarded
    }

    /// Takes every shell, leaving the set empty.
    pub fn drain(&mut self) -> Vec<Discarded> {
        let names: Vec<String> = self.shells.keys().cloned().collect();
        names
            .into_iter()
            .filter_map(|name| self.discard(&name))
            .collect()
    }

    /// Takes the retired shells, leaving none: each still has a process to end.
    pub fn take_retired(&mut self) -> Vec<Discarded> {
        std::mem::take(&mut self.retired)
    }

    /// Drops `handle` once its reader has finished, so the list holds only live ones.
    pub fn forget_retired(&mut self, handle: &Arc<AsyncMutex<Shell>>) {
        self.retired.retain(|retired| !retired.fits(handle));
    }
}

/// A shell taken out of its set: still running, and still this session's to end.
pub struct Discarded {
    /// The name every replacement of this shell shares, so a delete reaches it.
    name: String,
    handle: Arc<AsyncMutex<Shell>>,
    /// This machine's pid, recorded for a shell this machine owns and no other.
    local_pid: Option<u32>,
    door: Option<Arc<Door>>,
}

impl Discarded {
    /// The handle, for a caller that wants the shell itself.
    pub fn handle(&self) -> &Arc<AsyncMutex<Shell>> {
        &self.handle
    }

    /// This machine's pid, when this machine is what runs the shell.
    pub fn local_pid(&self) -> Option<u32> {
        self.local_pid
    }

    fn fits(&self, handle: &Arc<AsyncMutex<Shell>>) -> bool {
        Arc::ptr_eq(&self.handle, handle)
    }

    /// Ends what this shell left running, reporting what could be proved.
    ///
    /// # Invariant
    ///
    /// An endpoint shell is ended by the endpoint's own close, so this never waits
    /// for the lock its command holds, and its pid is never given to a helper on
    /// this machine. See `docs/notes/server-remote.md`.
    pub async fn end(self) -> CloseReport {
        if let Some(door) = &self.door {
            return door.close().await;
        }
        match self.handle.try_lock() {
            // Nothing runs there: closing through the shell cannot wait for work.
            Ok(mut shell) => shell.kill().await,
            Err(_) => match self.local_pid {
                Some(pid) => confirmed(crate::tools::detached::kill_tree(pid).await),
                // No local pid is kept for a peer's shell, and guessing one would
                // end an unrelated process here.
                None => CloseReport {
                    confirmed: false,
                    unknown: Some(
                        "the endpoint shell is busy and its control is gone; its end is not \
                         known"
                            .to_string(),
                    ),
                },
            },
        }
    }
}

/// A dropped shell is still ended: an owner that forgot cannot leave one running.
///
/// # Invariant
///
/// The net is the same single close, so it can only report to the log; a caller
/// that asked already has its own answer. See `docs/notes/server-remote.md`.
impl Drop for Discarded {
    fn drop(&mut self) {
        let Some(door) = self.door.as_ref().filter(|door| door.needs_end()) else {
            return;
        };
        let door = door.clone();
        // Only a shell that could have opened something is worth a task, and only
        // where there is a runtime left to run it.
        let Ok(handle) = tokio::runtime::Handle::try_current() else {
            return;
        };
        handle.spawn(async move {
            let report = door.close().await;
            if !report.confirmed {
                tracing::warn!(
                    unknown = ?report.unknown,
                    "a discarded shell's end could not be confirmed"
                );
            }
        });
    }
}

/// A tree kill on this machine either proved the tree gone or did not.
fn confirmed(proved: bool) -> CloseReport {
    CloseReport {
        confirmed: proved,
        unknown: (!proved).then(|| "the process tree could not be proved gone".to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    /// A set whose shells run on this machine's runner, standing in for a peer.
    fn endpoint_set(root: &Path, runner: std::path::PathBuf) -> ShellSet {
        ShellSet::new(
            Endpoint::Direct { runner },
            Some(root.to_path_buf()),
            Vec::new(),
        )
    }

    /// A discarded shell never opens itself afterwards: a peer-side shell with no
    /// owner is what that would leave.
    ///
    /// # Invariant
    ///
    /// Retirement is decided before the connection is made, so the command that
    /// arrives later is refused instead of opening a shell nothing will end. See
    /// `docs/notes/server-remote.md`.
    #[tokio::test]
    async fn a_discarded_endpoint_shell_never_opens_afterwards() {
        let Some(runner) = crate::remote::test_runner() else {
            return;
        };
        let root = tempfile::tempdir().expect("a directory the endpoint owns");
        let mut set = endpoint_set(root.path(), runner);
        let handle = set.get("late").expect("a shell");
        let discarded = set.discard("late").expect("the shell under that name");
        assert_eq!(
            discarded.local_pid(),
            None,
            "nothing was opened, so no pid is kept"
        );
        // Nothing was opened on the peer, so nothing is left there.
        let report = discarded.end().await;
        assert!(report.confirmed, "{:?}", report.unknown);

        let mut shell = handle.lock().await;
        let refused = shell.run("echo hi", Duration::from_secs(20), None).await;
        let message = match refused {
            Ok(outcome) => panic!("a discarded shell ran a command: {outcome:?}"),
            Err(e) => e.to_string(),
        };
        assert!(message.contains("discarded"), "{message}");
        assert_eq!(shell.pid(), 0, "no shell was opened on the peer");
    }

    /// A retired command is part of its name, not forgotten when a replacement starts.
    #[tokio::test]
    async fn deleting_a_name_ends_its_retired_shell_as_well() {
        let Some(runner) = crate::remote::test_runner() else {
            return;
        };
        let root = tempfile::tempdir().expect("a directory the endpoint owns");
        let mut set = endpoint_set(root.path(), runner);
        let handle = set.get("busy").expect("a shell");
        let running = handle.clone();
        let command = tokio::spawn(async move {
            let mut shell = running.lock().await;
            shell.run("sleep 30", Duration::from_secs(60), None).await
        });
        let control = tokio::time::timeout(Duration::from_secs(20), async {
            while set.control("busy").is_none() {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await;
        assert!(control.is_ok(), "the endpoint never opened the shell");
        assert!(set.retire("busy", &handle));
        set.get("busy").expect("a replacement");
        let discarded = set.discard_all("busy");
        assert_eq!(discarded.len(), 2, "the active and retired shell");
        for shell in discarded {
            assert!(shell.end().await.confirmed);
        }
        tokio::time::timeout(Duration::from_secs(30), command)
            .await
            .expect("the retired reader stopped")
            .expect("the retired reader joined")
            .expect("the retired command returned");
    }

    /// A busy endpoint shell is ended by its control: never by its lock, never by a
    /// pid on this machine.
    ///
    /// # Invariant
    ///
    /// The pid a peer reports is not a pid here, so it is not kept and not
    /// signalled; the endpoint's own close is the only thing that ends the shell.
    #[tokio::test]
    async fn a_busy_endpoint_shell_is_ended_without_its_lock_or_a_local_pid() {
        let Some(runner) = crate::remote::test_runner() else {
            return;
        };
        let root = tempfile::tempdir().expect("a directory the endpoint owns");
        let mut set = endpoint_set(root.path(), runner);
        let handle = set.get("busy").expect("a shell");

        // Held the way a running command holds it, for longer than this test waits.
        let busy = handle.clone();
        let command = tokio::spawn(async move {
            let mut shell = busy.lock().await;
            shell.run("sleep 30", Duration::from_secs(60), None).await
        });
        // Only a shell the endpoint has opened is worth ending.
        let control = tokio::time::timeout(Duration::from_secs(20), async {
            while set.control("busy").is_none() {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await;
        assert!(control.is_ok(), "the endpoint never opened the shell");

        let discarded = set.discard("busy").expect("the shell under that name");
        assert_eq!(
            discarded.local_pid(),
            None,
            "a peer's pid must not be kept as this machine's"
        );
        // The lock is held, so anything that waits for it would time out here.
        let report = tokio::time::timeout(Duration::from_secs(30), discarded.end())
            .await
            .expect("the end must not wait for a running command");
        assert!(report.confirmed, "{:?}", report.unknown);
        // The reader woke: its command cannot outlive the shell it ran in.
        let ended = tokio::time::timeout(Duration::from_secs(30), command).await;
        assert!(ended.is_ok(), "the reader waited for a shell that is gone");
    }
}
