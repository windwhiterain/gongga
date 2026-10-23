//! A row in SQLite plus the named shells that belong to it.
//! See `docs/notes/server-session.md`.

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
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Tuning {
    /// A roster entry, `provider/model`. `None` means "the server's own".
    pub llm: Option<String>,
    /// Compaction point as a percentage of the window; `None` is the default percent.
    pub compact_threshold: Option<u8>,
    /// How hard the model should think, in the roster's spelling; `None` is its default.
    pub thinking_effort: Option<String>,
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
    /// How many times the operator has cancelled; a queued turn compares it across the wait.
    cancels: AtomicU64,
    /// What every operation that reshapes this session queues behind, FIFO.
    gate: Arc<AsyncMutex<()>>,
    turns: AtomicU64,
    /// The tuning this session was given, behind a lock because it is read as a pair.
    tuning: Mutex<Tuning>,
    /// Set once the rows are gone, so a queued turn gives up instead of writing.
    deleted: AtomicBool,
    /// Set by a shutdown's `cancel`: the turn stops but its pending row stays.
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
    /// # Invariant
    ///
    /// Called once, on the way out: a shell that never exits keeps its reaper
    /// thread and the process alive.
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

    /// Whether someone holds the gate right now; only good for deciding to log a wait.
    pub fn busy(&self) -> bool {
        self.inner.gate.clone().try_lock_owned().is_err()
    }

    /// Waits until nothing else is reshaping this session, then holds the gate.
    ///
    /// # Invariant
    ///
    /// The guard is the point: dropping it lets the next one in, even if the
    /// holder panics.
    pub async fn claim(&self) -> OwnedMutexGuard<()> {
        self.inner.gate.clone().lock_owned().await
    }

    /// Cancels any in-flight turn and returns a fresh token for the next one.
    ///
    /// # Invariant
    ///
    /// The caller already holds `claim`; otherwise this stops the turn it is
    /// meant to wait behind.
    pub fn new_turn(&self) -> (u64, CancellationToken) {
        let mut guard = self.inner.cancel.lock().unwrap_or_else(|e| e.into_inner());
        guard.cancel();
        let token = CancellationToken::new();
        *guard = token.clone();
        let turn = self.inner.turns.fetch_add(1, Ordering::Relaxed) + 1;
        (turn, token)
    }

    /// Cancels the current turn without starting a new one; the turn is dropped.
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

    /// Cancels the turn but keeps its pending row for the next start to resume.
    pub fn cancel_and_preserve(&self) {
        self.inner.resumable.store(true, Ordering::SeqCst);
        self.cancel();
    }

    /// Whether a `cancel` asked this turn to survive the shutdown; clears it, for one turn.
    pub fn take_resumable(&self) -> bool {
        self.inner.resumable.swap(false, Ordering::SeqCst)
    }

    /// The working directory the default shell currently reports.
    pub async fn cwd(&self) -> PathBuf {
        // The guard is not `Send`, so scope it to a block before the first await.
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

/// The persistent shells of one session, locked synchronously: spawn and remove.
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

    /// The shared handle for `name`, spawning it fresh on first use after a removal.
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

    /// The shells that exist right now, in a stable order; naming one is what creates it.
    pub fn names(&self) -> Vec<String> {
        let mut names: Vec<String> = self.shells.keys().cloned().collect();
        names.sort();
        names
    }

    pub fn remove(&mut self, name: &str) -> bool {
        self.shells.remove(name).is_some()
    }

    /// Takes `handle` out of the set if it is still the one filed under `name`.
    ///
    /// # Invariant
    ///
    /// Compare by pointer: a replaced entry is not ours to remove.
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

    /// Retires `handle` and spawns a fresh shell in its place, keeping the name usable.
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
    /// Detached jobs, deliberately not owned by a session: they outlive everything.
    detached: DetachedRegistry,
    /// Variables for every session shell, so the model can reach this server via the CLI.
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

    /// Ends the shells of every live session, which the process needs to exit.
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
        let id = self
            .store
            .create_session(name, &self.model, &cwd.to_string_lossy())?;
        // Pin the model's entry: reasoning blobs replay only on the producing provider.
        let pinned = crate::providers::roster()
            .find_model_entry(&self.model)
            .map(|e| e.spec());
        if let Some(spec) = &pinned {
            let _ = self
                .store
                .set_session_tuning(id, Some(spec), None, None, &self.model);
        }
        self.materialise(SessionRow {
            id,
            name: name.to_string(),
            status: protocol::SessionStatus::Idle,
            model: self.model.clone(),
            llm: pinned,
            compact_threshold: None,
            thinking_effort: None,
            cwd: cwd.to_string_lossy().to_string(),
            created_at: 0,
            updated_at: 0,
            messages: 0,
        })
    }

    /// The handle if the session is already in memory, without spawning anything.
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
        let cwd = if cwd.exists() {
            cwd
        } else {
            std::env::current_dir()?
        };
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
                    // Prefer the pinned entry; a row that predates it is resolved from the roster.
                    llm: row.llm.clone().or_else(|| {
                        crate::providers::roster()
                            .find_model_entry(&row.model)
                            .map(|e| e.spec())
                    }),
                    compact_threshold: row.compact_threshold,
                    thinking_effort: row.thinking_effort,
                }),
                deleted: AtomicBool::new(false),
                resumable: AtomicBool::new(false),
            }),
        };
        self.live().insert(row.name, handle.clone());
        Ok(handle)
    }

    pub fn delete(&self, name: &str) -> anyhow::Result<bool> {
        // Mark first: a turn already waiting behind this deletion must see it and stop.
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

    /// `cancel` preserves the turn exactly once, so the next turn is not kept by accident.
    #[test]
    fn cancelling_for_shutdown_preserves_one_turn() {
        let h = handle();
        h.cancel_and_preserve();
        assert!(h.take_resumable());
        assert!(!h.take_resumable());
    }

    /// The second claim waits: two turns at once each compressed the same transcript.
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

    /// A turn queued behind `del_session` must notice the session is gone.
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
