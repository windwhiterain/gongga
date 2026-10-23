//! Sessions: a row in SQLite plus the named shells that belong to it.
//!
//! The SPEC keys a shell by `session_name/bash_name`, so a session owns a *set*
//! of persistent shells rather than one. `default` always exists; anything else
//! is created on first use and removed by the `del_bash` tool.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

use tokio::sync::Mutex as AsyncMutex;
use tokio::sync::OwnedMutexGuard;
use tokio_util::sync::CancellationToken;

use crate::config::BashConfig;
use crate::store::{SessionRow, Store};
use crate::tools::bash::BashShell;
use crate::tools::detached::DetachedRegistry;

/// The name of the shell every session starts with.
pub const DEFAULT_BASH: &str = "default";

/// What a session was told to use, as `set_session` left it.
///
/// Kept beside the session rather than in the agent because it is per-session
/// state: two sessions on one server can run different models at different
/// compaction points, and a turn reads this when it starts so a change applies
/// to the next turn instead of restarting anything.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Tuning {
    /// A roster entry, `provider/model`. `None` means "the server's own".
    pub llm: Option<String>,
    /// Compaction point as a percentage of the model's context window. `None`
    /// means the server-wide token budget.
    pub compact_threshold: Option<u8>,
}

/// A live session. Cheap to clone; the heavy parts are shared.
#[derive(Clone)]
pub struct SessionHandle {
    inner: Arc<SessionInner>,
}

struct SessionInner {
    id: i64,
    name: String,
    shells: Arc<Mutex<ShellSet>>,
    cancel: Mutex<CancellationToken>,
    /// How many times the operator has cancelled. A turn compares this before
    /// and after waiting its turn: if it moved, an `interrupt_session` arrived
    /// while the turn was still queued, and running it anyway would look like
    /// the cancel did nothing.
    cancels: AtomicU64,
    /// What every operation that reshapes this session queues behind.
    ///
    /// Two turns used to run side by side: both measured the transcript, both
    /// decided it was over budget, and both compressed the same thing -- twice
    /// the cost for one summary. A turn and `del_session` had the same problem
    /// from a different angle. Tokio's mutex is first-in-first-out, so this is
    /// a queue rather than a free-for-all: whoever asks first goes first.
    gate: Arc<AsyncMutex<()>>,
    turns: AtomicU64,
    /// The `--llm` and `--compact_threshold` this session was given.
    ///
    /// Behind a lock rather than atomics because it is read as a pair and
    /// written as a pair: a turn must not see a new model paired with the old
    /// budget.
    tuning: Mutex<Tuning>,
    /// Set once the session's rows are gone. A turn that was queued behind a
    /// `del_session` finds this when its turn comes and gives up instead of
    /// writing to a session that no longer exists.
    deleted: AtomicBool,
    /// Set by a `cancel` that shuts the server down: the turn stops, but its
    /// pending row stays so the next start picks the turn back up.
    /// `interrupt_session` leaves this false, because there "stop" means
    /// "drop it".
    resumable: AtomicBool,
}

impl SessionHandle {
    pub fn id(&self) -> i64 {
        self.inner.id
    }

    pub fn name(&self) -> &str {
        &self.inner.name
    }

    pub fn shells(&self) -> Arc<Mutex<ShellSet>> {
        self.inner.shells.clone()
    }

    /// Ends every shell this session owns and waits for each to be reaped.
    ///
    /// Called once, on the way out. A persistent shell is a child process that
    /// never exits by itself, so leaving one alive leaves its reaper thread
    /// alive too, and the process cannot finish shutting down. Nothing is lost
    /// by ending them: a shell is cheap to start again, and the transcript --
    /// where the real state lives -- is already written.
    pub async fn close_shells(&self) {
        let drained: Vec<Arc<AsyncMutex<BashShell>>> = {
            let mut set = self.inner.shells.lock().unwrap_or_else(|e| e.into_inner());
            set.drain()
        };
        tracing::info!(count = drained.len(), "closing shells");
        for handle in drained {
            // Turns are all finished by the time this runs, so this waits for a
            // lock nobody holds rather than for work.
            let mut shell = handle.lock().await;
            shell.kill().await;
        }
    }

    /// What this session was told to use. Read at the start of every turn.
    pub fn tuning(&self) -> Tuning {
        self.inner
            .tuning
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// Replaces the tuning. The caller has already written it down; this is the
    /// copy turns read.
    pub fn set_tuning(&self, tuning: Tuning) {
        *self.inner.tuning.lock().unwrap_or_else(|e| e.into_inner()) = tuning;
    }

    /// Whether someone is holding the gate right now. Only worth asking to
    /// decide whether to log a wait: the answer can change immediately after.
    pub fn busy(&self) -> bool {
        self.inner.gate.clone().try_lock_owned().is_err()
    }

    /// Waits until nothing else is reshaping this session, then holds it.
    ///
    /// Turns, `del_session` and anything else that reads the transcript and
    /// writes back a different version of it have to take this first. The
    /// guard is the whole point of the return value: dropping it is what lets
    /// the next one in, including if the holder panics.
    pub async fn claim(&self) -> OwnedMutexGuard<()> {
        self.inner.gate.clone().lock_owned().await
    }

    /// Cancels any in-flight turn and returns a fresh token for the next one.
    ///
    /// Callers must already hold `claim`: this cancels the running turn, so
    /// calling it from a turn that has not been let in yet would stop the one
    /// it is supposed to be waiting behind.
    pub fn new_turn(&self) -> (u64, CancellationToken) {
        let mut guard = self
            .inner
            .cancel
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        guard.cancel();
        let token = CancellationToken::new();
        *guard = token.clone();
        let turn = self.inner.turns.fetch_add(1, Ordering::Relaxed) + 1;
        (turn, token)
    }

    /// Cancels the current turn without starting a new one.
    ///
    /// The turn is dropped, not deferred: whoever cleans up after it clears its
    /// pending row, so the next start does not owe it anything.
    pub fn cancel(&self) {
        self.inner.cancels.fetch_add(1, Ordering::SeqCst);
        self.inner
            .cancel
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .cancel();
    }

    /// Whether a turn that is waiting its turn should bother starting.
    pub fn is_deleted(&self) -> bool {
        self.inner.deleted.load(Ordering::SeqCst)
    }

    /// Marks the session's rows gone, for `del_session` to call before it
    /// drops the handle.
    pub fn mark_deleted(&self) {
        self.inner.deleted.store(true, Ordering::SeqCst);
    }

    /// How many cancels have happened, so a queued turn can tell whether one
    /// happened while it was waiting.
    pub fn cancel_epoch(&self) -> u64 {
        self.inner.cancels.load(Ordering::SeqCst)
    }

    /// Cancels the current turn but keeps its pending row, so the next server
    /// resumes it. This is what `cancel` (the command) does: it stops work to
    /// shut down, not to throw the work away.
    pub fn cancel_and_preserve(&self) {
        self.inner.resumable.store(true, Ordering::SeqCst);
        self.cancel();
    }

    /// Whether a `cancel` asked for this turn to survive the shutdown, and
    /// clears the flag: it describes one turn only.
    pub fn take_resumable(&self) -> bool {
        self.inner.resumable.swap(false, Ordering::SeqCst)
    }

    /// The working directory the default shell currently reports.
    pub async fn cwd(&self) -> PathBuf {
        // `std::sync::MutexGuard` is not `Send`, so it has to be gone before
        // the first await. Scoping the lock to a block is what guarantees that.
        let shells = self.shells();
        let handle = {
            let guard = shells.lock().unwrap_or_else(|e| e.into_inner());
            guard.peek(DEFAULT_BASH)
        };
        match handle {
            Some(shell) => {
                let s = shell.lock().await;
                s.cwd().to_path_buf()
            }
            None => PathBuf::new(),
        }
    }
}

/// The persistent shells of one session.
///
/// Locking is `std::sync` because every operation here is synchronous: spawning
/// is a process launch and removing is a map delete. The per-shell lock the
/// tools actually await lives inside, one level down.
pub struct ShellSet {
    program: PathBuf,
    cwd: PathBuf,
    /// Variables every shell of this session starts with. See `BashShell::spawn`.
    env: Vec<(String, String)>,
    shells: HashMap<String, Arc<AsyncMutex<BashShell>>>,
}

impl ShellSet {
    fn new(program: PathBuf, cwd: PathBuf, env: Vec<(String, String)>) -> Self {
        Self {
            program,
            cwd,
            env,
            shells: HashMap::new(),
        }
    }

    /// The shared handle for `name`, spawning it on first use.
    ///
    /// Spawning is what makes `del_bash` meaningful: a shell that was removed
    /// comes back fresh (in the session directory) the next time it is named.
    pub fn get(&mut self, name: &str) -> anyhow::Result<Arc<AsyncMutex<BashShell>>> {
        if let Some(shell) = self.shells.get(name) {
            return Ok(shell.clone());
        }
        let shell = BashShell::spawn(&self.program, &self.cwd, &self.env)?;
        tracing::debug!(bash = %name, cwd = %self.cwd.display(), "shell started");
        let handle = Arc::new(AsyncMutex::new(shell));
        self.shells.insert(name.to_string(), handle.clone());
        Ok(handle)
    }

    /// The handle without spawning, for callers that must not create one.
    pub fn peek(&self, name: &str) -> Option<Arc<AsyncMutex<BashShell>>> {
        self.shells.get(name).cloned()
    }

    /// The shells that exist right now, in a stable order.
    ///
    /// `list_bash` reports live shells only; naming one is what creates it.
    pub fn names(&self) -> Vec<String> {
        let mut names: Vec<String> = self.shells.keys().cloned().collect();
        names.sort();
        names
    }

    pub fn remove(&mut self, name: &str) -> bool {
        self.shells.remove(name).is_some()
    }

    /// Takes `handle` out of the set if it is still the one filed under
    /// `name`, and reports whether it was.
    ///
    /// This is what happens when a command outlives its deadline: the shell
    /// keeps the command, the session gets a fresh shell under the same name,
    /// and the model can go on working instead of finding a shell that is
    /// still busy. The pointer comparison matters -- if the entry has already
    /// been replaced, removing it would take away somebody else's shell.
    pub fn retire(&mut self, name: &str, handle: &Arc<AsyncMutex<BashShell>>) -> bool {
        let mine = self
            .shells
            .get(name)
            .map(|h| Arc::ptr_eq(h, handle))
            .unwrap_or(false);
        if mine {
            self.shells.remove(name);
        }
        mine
    }

    /// Retires `handle` and puts a freshly spawned shell in its place.
    ///
    /// Replacing rather than merely removing is what keeps `check_bash` and
    /// `list_bash` honest: the name has to keep meaning "a shell you can use",
    /// otherwise the next tool call reports that the shell the model was just
    /// told about does not exist.
    pub fn replace(
        &mut self,
        name: &str,
        handle: &Arc<AsyncMutex<BashShell>>,
    ) -> anyhow::Result<bool> {
        if !self.retire(name, handle) {
            return Ok(false);
        }
        self.get(name)?;
        Ok(true)
    }

    /// Takes every shell, leaving the set empty.
    pub fn drain(&mut self) -> Vec<Arc<AsyncMutex<BashShell>>> {
        self.shells.drain().map(|(_, handle)| handle).collect()
    }
}

#[derive(Clone)]
pub struct SessionManager {
    store: Store,
    bash: BashConfig,
    model: String,
    /// Detached jobs, which are deliberately *not* owned by a session: they
    /// outlive shells, sessions and the server. Session-scoped tools still need
    /// to reach them, so they live here rather than inside a shell.
    detached: DetachedRegistry,
    /// Variables handed to every session shell, so the model can reach this
    /// server with the CLI instead of guessing at it.
    shell_env: Vec<(String, String)>,
    live: Arc<Mutex<HashMap<String, SessionHandle>>>,
}

impl SessionManager {
    pub fn new(
        store: Store,
        bash: BashConfig,
        model: String,
        detached: DetachedRegistry,
        shell_env: Vec<(String, String)>,
    ) -> Self {
        Self {
            store,
            bash,
            model,
            detached,
            shell_env,
            live: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// The detached jobs of every session.
    pub fn detached(&self) -> DetachedRegistry {
        self.detached.clone()
    }

    /// Ends the shells of every live session. See [`SessionHandle::close_shells`]
    /// for why the process cannot exit without this.
    pub async fn close_shells(&self) {
        let handles: Vec<SessionHandle> = self.live().values().cloned().collect();
        for handle in handles {
            handle.close_shells().await;
        }
    }

    fn live(&self) -> MutexGuard<'_, HashMap<String, SessionHandle>> {
        self.live.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Creates a session, or returns the existing one unchanged.
    pub fn ensure(&self, name: &str, cwd: Option<&str>) -> anyhow::Result<SessionHandle> {
        if let Some(h) = self.live().get(name) {
            return Ok(h.clone());
        }
        if let Some(row) = self.store.get_session(name)? {
            return self.materialise(row);
        }
        let cwd = match cwd {
            Some(p) => PathBuf::from(p),
            None => std::env::current_dir()?,
        };
        let id = self.store.create_session(name, &self.model, &cwd.to_string_lossy())?;
        // Pin the entry the session starts on, when the roster knows that
        // model. A session is not "whatever the server's default is today": its
        // reasoning items carry blobs only the producing provider can replay,
        // so a later change of default must not silently move it.
        let pinned = crate::providers::roster()
            .find_model_entry(&self.model)
            .map(|e| e.spec());
        if let Some(spec) = &pinned {
            let _ = self
                .store
                .set_session_tuning(id, Some(spec), None, &self.model);
        }
        self.materialise(SessionRow {
            id,
            name: name.to_string(),
            status: protocol::SessionStatus::Idle,
            model: self.model.clone(),
            llm: pinned,
            compact_threshold: None,
            cwd: cwd.to_string_lossy().to_string(),
            created_at: 0,
            updated_at: 0,
            messages: 0,
        })
    }

    /// The handle if the session is already in memory, without creating it.
    ///
    /// `del_session` needs this: it only has to wait for a session that has
    /// work in flight, and materialising one in order to delete it would start
    /// shells for nothing.
    pub fn peek(&self, name: &str) -> Option<SessionHandle> {
        self.live().get(name).cloned()
    }

    /// Looks a session up, spawning its shells on first use.
    pub fn get(&self, name: &str) -> anyhow::Result<SessionHandle> {
        if let Some(h) = self.live().get(name) {
            return Ok(h.clone());
        }
        let row = self
            .store
            .get_session(name)?
            .ok_or_else(|| anyhow::anyhow!("no session named {name:?}"))?;
        self.materialise(row)
    }

    fn materialise(&self, row: SessionRow) -> anyhow::Result<SessionHandle> {
        let program = self
            .bash
            .program
            .clone()
            .ok_or_else(|| anyhow::anyhow!("no bash available; set [tools.bash] program"))?;
        let cwd = PathBuf::from(&row.cwd);
        let cwd = if cwd.exists() { cwd } else { std::env::current_dir()? };
        let mut shells = ShellSet::new(program, cwd.clone(), self.shell_env.clone());
        // The default shell is part of the session's identity, so it exists
        // before anything asks for it.
        shells.get(DEFAULT_BASH)?;
        tracing::debug!(session = %row.name, model = %row.model, cwd = %cwd.display(), "session shells ready");
        let handle = SessionHandle {
            inner: Arc::new(SessionInner {
                id: row.id,
                name: row.name.clone(),
                shells: Arc::new(Mutex::new(shells)),
                cancel: Mutex::new(CancellationToken::new()),
                cancels: AtomicU64::new(0),
                gate: Arc::new(AsyncMutex::new(())),
                turns: AtomicU64::new(0),
                tuning: Mutex::new(Tuning {
                    // `llm` is what the session was pinned to; a row that
                    // predates it still knows its model, and the roster is what
                    // turns that name back into a provider. Only a model the
                    // roster has never heard of falls back to the server's own.
                    llm: row.llm.clone().or_else(|| {
                        crate::providers::roster()
                            .find_model_entry(&row.model)
                            .map(|e| e.spec())
                    }),
                    compact_threshold: row.compact_threshold,
                }),
                deleted: AtomicBool::new(false),
                resumable: AtomicBool::new(false),
            }),
        };
        self.live().insert(row.name, handle.clone());
        Ok(handle)
    }

    pub fn delete(&self, name: &str) -> anyhow::Result<bool> {
        // Say it before the rows go, so a turn already waiting behind this
        // deletion sees it and stops instead of writing into the void.
        if let Some(handle) = self.live().get(name) {
            handle.mark_deleted();
        }
        self.live().remove(name);
        self.store.delete_session(name)
    }

    pub fn list(&self) -> anyhow::Result<Vec<SessionRow>> {
        self.store.list_sessions()
    }

    /// Every session with a live handle, i.e. one whose shells exist in memory.
    ///
    /// Used by `cancel`, which has to stop turns that are actually running
    /// rather than every row in the database.
    pub fn live_sessions(&self) -> Vec<SessionHandle> {
        self.live().values().cloned().collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicBool;
    use std::time::Duration;

    /// A handle with no shells: the flag under test does not touch them.
    fn handle() -> SessionHandle {
        SessionHandle {
            inner: Arc::new(SessionInner {
                id: 1,
                name: "s".to_string(),
                shells: Arc::new(Mutex::new(ShellSet::new(
                    PathBuf::from("bash"),
                    std::env::temp_dir(),
                    vec![],
                ))),
                cancel: Mutex::new(CancellationToken::new()),
                cancels: AtomicU64::new(0),
                gate: Arc::new(AsyncMutex::new(())),
                turns: AtomicU64::new(0),
                tuning: Mutex::new(Tuning::default()),
                deleted: AtomicBool::new(false),
                resumable: AtomicBool::new(false),
            }),
        }
    }

    /// `interrupt_session` means "stop and drop it": nothing asks for the turn
    /// to resume.
    #[test]
    fn a_plain_cancel_does_not_preserve_the_turn() {
        let h = handle();
        h.cancel();
        assert!(!h.take_resumable());
    }

    /// `cancel` shuts the server down so the turn can come back, and it says so
    /// exactly once: the next turn is not preserved by accident.
    #[test]
    fn cancelling_for_shutdown_preserves_one_turn() {
        let h = handle();
        h.cancel_and_preserve();
        assert!(h.take_resumable());
        assert!(!h.take_resumable());
    }

    /// What queuing means: the second caller does not get in until the first
    /// one is done. Two turns on one session used to run at once and each
    /// compressed the same transcript.
    #[tokio::test]
    async fn the_second_claim_waits_for_the_first() {
        let h = handle();
        let first = h.claim().await;
        let waiting = Arc::new(AtomicBool::new(false));

        let other = h.clone();
        let flag = waiting.clone();
        let task = tokio::spawn(async move {
            flag.store(true, Ordering::SeqCst);
            let _second = other.claim().await;
            flag.store(false, Ordering::SeqCst);
        });

        // Give the spawned task time to reach the queue and stop there.
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(waiting.load(Ordering::SeqCst), "the second claim waited");

        drop(first);
        task.await.expect("the queued claim finishes");
        assert!(!waiting.load(Ordering::SeqCst));
    }

    /// A turn queued behind a `del_session` has to notice the session is gone
    /// when its turn comes, instead of writing into a deleted session.
    #[test]
    fn deletion_is_visible_to_a_queued_turn() {
        let h = handle();
        assert!(!h.is_deleted());
        h.mark_deleted();
        assert!(h.is_deleted());
    }

    /// A cancel while queued is visible to the turn (see `turn_inner`).
    #[tokio::test]
    async fn a_cancel_while_queued_is_visible_to_the_turn() {
        let h = handle();
        let _first = h.claim().await;
        let epoch = h.cancel_epoch();
        h.cancel();
        assert_ne!(h.cancel_epoch(), epoch);
    }
}
