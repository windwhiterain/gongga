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
    /// The model this session runs on, `provider/model_name`. `None` means "the
    /// server's own".
    pub model: Option<String>,
    /// The context level a notice is sent at, as a percentage of the window;
    /// `None` means the default share.
    pub context_limit: Option<u8>,
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
    /// Shared rather than embedded: every tool call waiting holds a copy of the count.
    inbox: Arc<AtomicU64>,
    /// The measured context a notice was last sent at; 0 is never.
    context_asked: AtomicU64,
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

    /// Ends every shell this session owns, retired ones included, and waits for
    /// each to be reaped.
    ///
    /// # Invariant
    ///
    /// Called once, on the way out: a shell that never exits keeps its reaper
    /// thread and the process alive.
    pub async fn close_shells(&self) {
        let (drained, retired): (Vec<_>, Vec<_>) = {
            let mut set = self.inner.shells.lock().unwrap_or_else(|e| e.into_inner());
            (set.drain(), set.take_retired())
        };
        tracing::info!(
            count = drained.len(),
            retired = retired.len(),
            "closing shells"
        );
        for handle in drained {
            // Turns are all finished by the time this runs, so this waits for a
            // lock nobody holds rather than for work.
            let mut shell = handle.lock().await;
            shell.kill().await;
        }
        for (pid, handle) in retired {
            // A retired shell is held by the task reading it: the pid ends the
            // command and that reader stops.
            match handle.try_lock() {
                Ok(mut shell) => shell.kill().await,
                Err(_) => {
                    crate::tools::detached::kill_tree(pid).await;
                }
            }
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

    /// Records that a message landed which no running turn can read yet.
    ///
    /// See `docs/notes/server-session.md`.
    pub fn note_message(&self) {
        self.inner.inbox.fetch_add(1, Ordering::SeqCst);
    }

    /// Records that the loop told this session its context reached the threshold.
    ///
    /// # Invariant
    ///
    /// True only once per `upto`: the agent is told when the threshold is crossed,
    /// not on every step that stays above it, and it is told again only after it
    /// has folded something -- the point at which the advice changed.
    pub fn note_context_asked(&self, tokens: u64, budget: u64) -> bool {
        // A quarter of the budget is the granule: small enough that a session
        // still drifting upward is told again before it doubles, large enough
        // that a turn spent just over the line is not nagged on every step.
        let granule = (budget / 4).max(1);
        let mut current = self.inner.context_asked.load(Ordering::SeqCst);
        loop {
            // Never told, or told at a size this has since fallen well below:
            // a handover empties the session and re-arms the notice.
            let fresh = current == 0 || tokens.saturating_add(granule) < current;
            // Or it has grown a granule past what was announced.
            let grown = tokens > current.saturating_add(granule);
            if !fresh && !grown {
                return false;
            }
            match self.inner.context_asked.compare_exchange(
                current,
                tokens,
                Ordering::SeqCst,
                Ordering::SeqCst,
            ) {
                Ok(_) => return true,
                Err(actual) => current = actual,
            }
        }
    }

    /// Takes the count, so a tool call can see whether one arrived while it waited.
    ///
    /// See `docs/notes/server-session.md`.
    pub fn inbox_signal(&self) -> InboxSignal {
        let epoch = self.inner.inbox.clone();
        let since = epoch.load(Ordering::SeqCst);
        InboxSignal { epoch, since }
    }

    /// Whether someone holds the gate right now; only good for deciding to log a wait.
    pub fn busy(&self) -> bool {
        self.inner.gate.clone().try_lock_owned().is_err()
    }

    /// Atomically claims the gate if it is free, before the caller spawns any work.
    pub fn try_claim(&self) -> Option<OwnedMutexGuard<()>> {
        self.inner.gate.clone().try_lock_owned().ok()
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

    /// Where the session's own shell last said it is, without taking its lock.
    ///
    /// See `docs/notes/server-session.md`.
    pub fn cwd(&self) -> PathBuf {
        self.shells()
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .reported_cwd()
    }
}

/// Whether a message landed for this session since this signal was taken.
///
/// See `docs/notes/server-session.md`.
pub struct InboxSignal {
    epoch: Arc<AtomicU64>,
    since: u64,
}

impl InboxSignal {
    /// One built around a counter a test can bump, since only a session owns one.
    #[cfg(test)]
    pub(crate) fn around(epoch: Arc<AtomicU64>) -> Self {
        let since = epoch.load(Ordering::SeqCst);
        Self { epoch, since }
    }

    /// True if a message arrived after this signal was taken.
    pub fn arrived(&self) -> bool {
        self.epoch.load(Ordering::SeqCst) != self.since
    }
}

/// The persistent shells of one session, locked synchronously: spawn and remove.
pub struct ShellSet {
    program: PathBuf,
    cwd: PathBuf,
    /// Where the default shell last said it was, mirrored out cell by cell;
    /// see `docs/notes/server-session.md`.
    reported: Arc<Mutex<PathBuf>>,
    /// Variables every shell of this session starts with. See `BashShell::spawn`.
    env: Vec<(String, String)>,
    shells: HashMap<String, Arc<AsyncMutex<BashShell>>>,
    /// Each live shell's pid, kept so a retired one can be ended without its lock.
    pids: HashMap<String, u32>,
    /// Commands handed to the background: out of `shells`, still this session's processes.
    retired: Vec<(u32, Arc<AsyncMutex<BashShell>>)>,
}

impl ShellSet {
    pub(crate) fn new(program: PathBuf, cwd: PathBuf, env: Vec<(String, String)>) -> Self {
        Self {
            program,
            cwd: cwd.clone(),
            reported: Arc::new(Mutex::new(cwd)),
            env,
            shells: HashMap::new(),
            pids: HashMap::new(),
            retired: Vec::new(),
        }
    }

    /// Where the session's own shell last said it is; read without its lock.
    ///
    /// See `docs/notes/server-session.md`.
    pub fn reported_cwd(&self) -> PathBuf {
        self.reported
            .lock()
            .map(|guard| guard.clone())
            .unwrap_or_default()
    }

    /// The shared handle for `name`, spawning it fresh on first use after a removal.
    pub fn get(&mut self, name: &str) -> anyhow::Result<Arc<AsyncMutex<BashShell>>> {
        if let Some(shell) = self.shells.get(name) {
            return Ok(shell.clone());
        }
        let shell = BashShell::spawn(&self.program, &self.cwd, &self.env)?;
        tracing::debug!(bash = %name, cwd = %self.cwd.display(), "shell started");
        // Only `default` is mirrored: a replacement brings a cell of its own.
        if name == DEFAULT_BASH {
            self.reported = shell.cwd_cell();
        }
        let pid = shell.pid();
        let handle = Arc::new(AsyncMutex::new(shell));
        self.shells.insert(name.to_string(), handle.clone());
        self.pids.insert(name.to_string(), pid);
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
        self.pids.remove(name);
        self.shells.remove(name).is_some()
    }

    /// Takes `handle` out of the set if it is still the one filed under `name`.
    ///
    /// # Invariant
    ///
    /// Compare by pointer: a replaced entry is not ours to remove. The retired
    /// shell keeps running its outstanding command, so its pid is kept too: it is
    /// still a process this session spawned, and teardown ends it.
    pub fn retire(&mut self, name: &str, handle: &Arc<AsyncMutex<BashShell>>) -> bool {
        let mine = self
            .shells
            .get(name)
            .map(|h| Arc::ptr_eq(h, handle))
            .unwrap_or(false);
        if mine {
            self.shells.remove(name);
            if let Some(pid) = self.pids.remove(name) {
                self.retired.push((pid, handle.clone()));
            }
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
        self.pids.clear();
        self.shells.drain().map(|(_, handle)| handle).collect()
    }

    /// Takes the retired shells, leaving none: each still has a process to end.
    pub fn take_retired(&mut self) -> Vec<(u32, Arc<AsyncMutex<BashShell>>)> {
        std::mem::take(&mut self.retired)
    }

    /// Drops `handle` once its reader has finished, so the list holds only live ones.
    pub fn forget_retired(&mut self, handle: &Arc<AsyncMutex<BashShell>>) {
        self.retired
            .retain(|(_, retired)| !Arc::ptr_eq(retired, handle));
    }
}

#[derive(Clone)]
pub struct SessionManager {
    store: Store,
    bash: BashConfig,
    /// The model every session here runs on unless it named one, as
    /// `provider/model_name` when the roster can place it.
    model: String,
    /// Detached jobs outlive shell, cancel and restart; `del_session` still deletes
    /// that session's bash.
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
        // Hold the live lock through the build: competing callers must not each
        // materialise and then overwrite one name.
        let mut live = self.live();
        if let Some(h) = live.get(name) {
            return Ok(h.clone());
        }
        if let Some(row) = self.store.get_session(name)? {
            return self.materialise_locked(row, &mut live);
        }
        let cwd = match cwd {
            Some(p) => PathBuf::from(p),
            None => std::env::current_dir()?,
        };
        let id = self
            .store
            .create_session(name, &self.model, &cwd.to_string_lossy())?;
        // Pin the model itself: reasoning blobs replay only where they were produced.
        let pinned = crate::providers::roster()
            .place(&self.model)
            .map(|e| e.model);
        if let Some(model) = &pinned {
            let _ = self
                .store
                .set_session_tuning(id, Some(model), None, None, &self.model);
        }
        self.materialise_locked(
            SessionRow {
                id,
                name: name.to_string(),
                status: protocol::SessionStatus::Idle,
                model: pinned.unwrap_or_else(|| self.model.clone()),
                context_limit: None,
                thinking_effort: None,
                cwd: cwd.to_string_lossy().to_string(),
                created_at: 0,
                updated_at: 0,
                messages: 0,
                // A session that just came into existence has measured nothing.
                usage: protocol::Usage::default(),
            },
            &mut live,
        )
    }

    /// The handle if the session is already in memory, without spawning anything.
    pub fn peek(&self, name: &str) -> Option<SessionHandle> {
        self.live().get(name).cloned()
    }

    /// Looks a session up, spawning its shells on first use.
    pub fn get(&self, name: &str) -> anyhow::Result<SessionHandle> {
        let mut live = self.live();
        if let Some(h) = live.get(name) {
            return Ok(h.clone());
        }
        let row = self
            .store
            .get_session(name)?
            .ok_or_else(|| anyhow::anyhow!("no session named {name:?}"))?;
        self.materialise_locked(row, &mut live)
    }

    fn materialise(&self, row: SessionRow) -> anyhow::Result<SessionHandle> {
        let mut live = self.live();
        self.materialise_locked(row, &mut live)
    }

    fn materialise_locked(
        &self,
        row: SessionRow,
        live: &mut HashMap<String, SessionHandle>,
    ) -> anyhow::Result<SessionHandle> {
        if let Some(h) = live.get(&row.name) {
            return Ok(h.clone());
        }
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
        let mut env = self.shell_env.clone();
        // A shell cannot learn its own session's name from the outside.
        env.push(("GONGGA_SESSION".to_string(), row.name.clone()));
        let mut shells = ShellSet::new(program, cwd.clone(), env);
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
                inbox: Arc::new(AtomicU64::new(0)),
                context_asked: AtomicU64::new(0),
                gate: Arc::new(AsyncMutex::new(())),
                turns: AtomicU64::new(0),
                tuning: Mutex::new(Tuning {
                    // The row's own spelling first; a row that only knows the name
                    // still finds its provider through the roster.
                    model: Some(
                        crate::providers::roster()
                            .place(&row.model)
                            .map(|e| e.model)
                            .unwrap_or_else(|| row.model.clone()),
                    ),
                    context_limit: row.context_limit,
                    thinking_effort: row.thinking_effort,
                }),
                deleted: AtomicBool::new(false),
                resumable: AtomicBool::new(false),
            }),
        };
        live.insert(row.name, handle.clone());
        Ok(handle)
    }

    pub fn delete(&self, name: &str) -> anyhow::Result<bool> {
        // Mark first: a turn already waiting behind this deletion must see it and stop.
        if let Some(handle) = self.live().get(name) {
            handle.mark_deleted();
        }
        self.live().remove(name);
        // Files go with it, if nothing else names them: ask the rows.
        let orphans = self.store.orphaned_blobs(name).unwrap_or_default();
        let dropped = self.store.delete_session(name)?;
        if dropped && !orphans.is_empty() {
            crate::blobs::drop_all(&orphans);
        }
        Ok(dropped)
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

    /// Everything a test here does not vary, wrapped around the shells it might.
    fn session(name: &str, shells: ShellSet) -> SessionHandle {
        SessionHandle {
            inner: Arc::new(SessionInner {
                id: 1,
                name: name.to_string(),
                shells: Arc::new(Mutex::new(shells)),
                cancel: Mutex::new(CancellationToken::new()),
                cancels: AtomicU64::new(0),
                inbox: Arc::new(AtomicU64::new(0)),
                context_asked: AtomicU64::new(0),
                gate: Arc::new(AsyncMutex::new(())),
                turns: AtomicU64::new(0),
                tuning: Mutex::new(Tuning::default()),
                deleted: AtomicBool::new(false),
                resumable: AtomicBool::new(false),
            }),
        }
    }

    /// A handle with no shells: the flag under test does not touch them.
    fn handle() -> SessionHandle {
        session(
            "s",
            ShellSet::new(PathBuf::from("bash"), std::env::temp_dir(), vec![]),
        )
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

    /// Pre-claiming is synchronous and keeps the gate until the guard is dropped.
    #[test]
    fn try_claim_atomically_takes_a_free_gate() {
        let h = handle();
        assert!(!h.busy());
        let first = h.try_claim().expect("the free gate can be claimed");
        assert!(h.busy());
        assert!(h.try_claim().is_none(), "a busy gate cannot be claimed");
        drop(first);
        assert!(!h.busy());
        let second = h.try_claim().expect("dropping the guard frees the gate");
        assert!(h.busy());
        drop(second);
    }

    /// The notice is sent once per transcript state, not once per step.
    ///
    /// # Invariant
    ///
    /// A turn that stays above the threshold takes several steps; repeating the
    /// notice on each one would bury the work it is asking about. Three things
    /// earn another one: growth of a quarter of the budget, a handover that
    /// empties the session, and a context that was cut back below the last size.
    #[test]
    fn a_context_notice_is_sent_once_per_crossing() {
        let shells = ShellSet::new(PathBuf::from("bash"), std::env::temp_dir(), vec![]);
        let handle = session("s", shells);
        let budget = 200;
        assert!(
            handle.note_context_asked(budget, budget),
            "the first crossing is announced"
        );
        assert!(
            !handle.note_context_asked(budget + 10, budget),
            "the same crossing is not announced twice"
        );
        assert!(
            handle.note_context_asked(budget + 60, budget),
            "a quarter past the announced size is a new crossing"
        );
        assert!(
            !handle.note_context_asked(budget + 61, budget),
            "and only once"
        );
        assert!(
            handle.note_context_asked(budget, budget),
            "a handover empties the session, which re-arms the notice"
        );
    }

    /// A signal sees the messages that landed after it was taken, and never the same one twice.
    #[test]
    fn an_inbox_signal_sees_only_what_arrived_after_it_was_taken() {
        let h = handle();
        let taken = h.inbox_signal();
        assert!(!taken.arrived(), "nothing had arrived");
        h.note_message();
        assert!(
            taken.arrived(),
            "a message landed after the signal was taken"
        );
        // The next call takes a fresh signal, so it is not told about that one again.
        assert!(!h.inbox_signal().arrived());
    }

    /// Concurrent entry points must share both the session state and its turn gate.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn concurrent_materialisation_is_single_flight() {
        let Some(program) = crate::config::detect_git_bash() else {
            return;
        };
        let dir = tempfile::tempdir().expect("creating a test directory");
        let store = Store::open(&dir.path().join("g.db")).expect("opening the store");
        store
            .create_session("shared", "m", &dir.path().to_string_lossy())
            .expect("creating the session row");
        let row = store
            .get_session("shared")
            .expect("reading the session row")
            .expect("the session row exists");
        let detached =
            DetachedRegistry::new(store.clone(), None, dir.path().join("detached"), vec![]);
        let manager = SessionManager::new(
            store,
            BashConfig {
                program: Some(program),
                timeout: Duration::from_secs(1),
                max_output_chars: 0,
            },
            "m".into(),
            detached,
            vec![],
        );

        const CALLERS: usize = 9;
        let barrier = Arc::new(std::sync::Barrier::new(CALLERS));
        let mut tasks = Vec::new();
        for caller in 0..CALLERS {
            let manager = manager.clone();
            let barrier = barrier.clone();
            let row = row.clone();
            tasks.push(tokio::task::spawn_blocking(move || {
                barrier.wait();
                match caller % 3 {
                    0 => manager.ensure("shared", None),
                    1 => manager.get("shared"),
                    _ => manager.materialise(row),
                }
            }));
        }

        let mut handles = Vec::new();
        for task in tasks {
            handles.push(
                task.await
                    .expect("joining a concurrent caller")
                    .expect("materialising the session"),
            );
        }
        let first = &handles[0];
        for handle in &handles[1..] {
            assert!(
                Arc::ptr_eq(&first.inner, &handle.inner),
                "every caller must receive the same SessionInner"
            );
            assert!(
                Arc::ptr_eq(&first.inner.gate, &handle.inner.gate),
                "every caller must receive the same turn gate"
            );
        }
        assert_eq!(manager.live_sessions().len(), 1);
        manager.close_shells().await;
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

    /// A shell held by a command must not hold up reading where a session is.
    ///
    /// See `docs/notes/server-session.md`.
    #[tokio::test]
    async fn the_session_cwd_does_not_wait_for_a_running_command() {
        let Some(program) = crate::config::detect_git_bash() else {
            return;
        };
        let root = std::env::temp_dir();
        let mut shells = ShellSet::new(program, root.clone(), vec![]);
        let shell = shells
            .get(DEFAULT_BASH)
            .expect("spawning the default shell");
        let session = session("s", shells);

        // Hold the shell the way a running command does.
        let held = shell.lock().await;
        let started = std::time::Instant::now();
        let cwd = session.cwd();
        let waited = started.elapsed();
        drop(held);
        assert!(
            waited < Duration::from_secs(1),
            "reading the cwd waited {waited:?} on the command holding the shell"
        );
        assert_eq!(cwd, root, "nothing has run, so this is where it started");
    }

    /// A backgrounded command's shell still belongs to the session: teardown ends it
    /// without waiting for its lock.
    #[tokio::test]
    async fn a_retired_shell_is_ended_without_waiting_for_its_lock() {
        let Some(program) = crate::config::detect_git_bash() else {
            return;
        };
        let root = std::env::temp_dir();
        let mut shells = ShellSet::new(program, root.clone(), vec![]);
        let shell = shells
            .get(DEFAULT_BASH)
            .expect("spawning the default shell");
        // As the reader holds it: the lock is busy, so teardown kills by pid rather
        // than waiting on the lock.
        let held = shell.lock().await;
        assert!(shells.retire(DEFAULT_BASH, &shell), "the shell is retired");
        let session = session("s", shells);
        let closed = tokio::time::timeout(Duration::from_secs(10), session.close_shells()).await;
        assert!(closed.is_ok(), "teardown waited for a retired shell's lock");
        drop(held);
    }

    /// The mirror has to follow the shell, or it answers with where the session
    /// started rather than where it is.
    #[tokio::test]
    async fn a_cd_in_the_default_shell_reaches_the_session_cwd() {
        let Some(program) = crate::config::detect_git_bash() else {
            return;
        };
        let root = std::env::temp_dir();
        let mut shells = ShellSet::new(program, root.clone(), vec![]);
        let shell = shells
            .get(DEFAULT_BASH)
            .expect("spawning the default shell");
        let session = session("s", shells);

        let leaf = format!("gongga-cwd-{}", std::process::id());
        let target = root.join(&leaf);
        std::fs::create_dir_all(&target).expect("creating a directory to cd into");
        let mut bash = shell.lock().await;
        bash.run(
            &format!("cd '{}'", crate::tools::bash::msys_path(&target)),
            Duration::from_secs(10),
            None,
        )
        .await
        .expect("running cd");
        drop(bash);
        let reported = session.cwd();
        let _ = std::fs::remove_dir(&target);
        assert_eq!(
            reported.file_name().and_then(|n| n.to_str()),
            Some(leaf.as_str()),
            "the cd did not reach the session cwd: {reported:?}"
        );
    }
}
