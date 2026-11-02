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

    /// Runs one command without following it with a `pwd`, for a caller that does
    /// not read the directory afterwards.
    pub async fn run_without_cwd(
        &mut self,
        command: &str,
        timeout: std::time::Duration,
    ) -> anyhow::Result<BashOutcome> {
        match self {
            Shell::Local(shell) => shell.run(command, timeout, None).await,
            Shell::Remote(shell) => shell.run_without_cwd(command, timeout).await,
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
