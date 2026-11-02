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
use crate::remote::endpoint::Endpoint;
use crate::remote::shell_set::ShellSet;
use crate::store::{SessionRow, Store};
use crate::tools::detached::DetachedRegistry;

/// The name of the shell every session starts with.
pub const DEFAULT_BASH: &str = "default";

/// The shell a target keeps only to answer directory listings for completion.
///
/// # Invariant
///
/// Named so it can never collide with a shell the model asks for: every tool
/// argument naming a shell is the model's own, and this one is not a session's.
const LISTING_SHELL: &str = "..completion";

/// What a session was told to use, as `set_session` left it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Tuning {
    /// The model this session runs on, `provider/model_name`. `None` means "the
    /// server's own".
    pub model: Option<String>,
    /// The context level a notice is sent at, as a percentage of the window;
    /// `None` means the default share.
    pub context_limit: Option<u8>,
    /// How hard the model should think, in the roster's spelling, resolved when
    /// `set_session` wrote it; `None` is a model that publishes no level.
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
    /// The rows, for the directory an endpoint shell reports.
    store: Store,
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
    /// Set before the shells are ended: a command that stops now was stopped, and
    /// a notice about it would be news about the server's own exit.
    closing: Arc<AtomicBool>,
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
        // Before the first shell goes: whatever ends from here was ended by us.
        self.inner.closing.store(true, Ordering::SeqCst);
        let (drained, retired): (Vec<_>, Vec<_>) = {
            let mut set = self.inner.shells.lock().unwrap_or_else(|e| e.into_inner());
            (set.drain(), set.take_retired())
        };
        tracing::info!(
            count = drained.len(),
            retired = retired.len(),
            "closing shells"
        );
        for discarded in drained.into_iter().chain(retired) {
            // A retired shell is held by the task reading it, so its end goes
            // through its own control rather than through its lock.
            let report = discarded.end().await;
            if !report.confirmed {
                tracing::warn!(
                    session = %self.inner.name,
                    unknown = ?report.unknown,
                    "a shell's end could not be confirmed"
                );
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

    /// The flag a tool call reads to know its shell is being ended by an exit.
    pub fn closing_signal(&self) -> Arc<AtomicBool> {
        self.inner.closing.clone()
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

    /// Keeps the row's directory equal to where an endpoint put this session's shell.
    ///
    /// # Invariant
    ///
    /// Only a session whose shells are not this machine's writes it, and only once a
    /// shell has connected: the row is what the next start hands back to the endpoint.
    /// A local row stays the directory the session was made in. See
    /// `docs/notes/server-remote.md`.
    pub fn adopt_reported_cwd(&self) {
        let (endpoint_is_remote, cwd) = {
            let shells = self.inner.shells.lock().unwrap_or_else(|e| e.into_inner());
            (shells.endpoint().is_remote(), shells.reported_cwd())
        };
        if !endpoint_is_remote || cwd.as_os_str().is_empty() {
            return;
        }
        if let Err(e) = self
            .inner
            .store
            .set_session_cwd(&self.inner.name, &cwd.to_string_lossy())
        {
            tracing::warn!(
                session = %self.inner.name,
                "could not record where the endpoint put this session: {e}"
            );
        }
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
    /// True if a message arrived after this signal was taken.
    pub fn arrived(&self) -> bool {
        self.epoch.load(Ordering::SeqCst) != self.since
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
    /// The named peers a session may run on. See `docs/notes/server-remote.md`.
    targets: Vec<crate::config::TargetConfig>,
    live: Arc<Mutex<HashMap<String, SessionHandle>>>,
    /// One shell per target, opened only to list a directory for completion.
    ///
    /// # Invariant
    ///
    /// Kept apart from every session's shells and never locked while `live` is:
    /// a directory listing is answered while a turn is running, and a Tab must not
    /// queue behind one. See `docs/notes/server-remote.md`.
    listing_shells: Arc<Mutex<HashMap<String, ShellSet>>>,
}

impl SessionManager {
    pub fn new(
        store: Store,
        bash: BashConfig,
        model: String,
        detached: DetachedRegistry,
        shell_env: Vec<(String, String)>,
        targets: Vec<crate::config::TargetConfig>,
    ) -> Self {
        Self {
            store,
            bash,
            model,
            detached,
            shell_env,
            targets,
            live: Arc::new(Mutex::new(HashMap::new())),
            listing_shells: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// The entries of one directory, as a `--cwd` completion would offer them.
    ///
    /// # Invariant
    ///
    /// Answers bare names and whether each is a directory, never a path: the
    /// caller keeps the part of the path already typed and formats the candidate,
    /// so nothing here invents a spelling for a filesystem it cannot see.
    pub async fn list_paths(
        &self,
        target: Option<&str>,
        directory: &str,
    ) -> anyhow::Result<Vec<(String, bool)>> {
        let endpoint = self.endpoint_for(target)?;
        if !endpoint.is_remote() {
            let root = match directory.trim() {
                "" => std::env::current_dir()?,
                path => PathBuf::from(path),
            };
            let mut entries: Vec<(String, bool)> = std::fs::read_dir(root)?
                .flatten()
                .map(|entry| {
                    (
                        entry.file_name().to_string_lossy().into_owned(),
                        entry.file_type().map(|t| t.is_dir()).unwrap_or(false),
                    )
                })
                .collect();
            entries.sort();
            return Ok(entries);
        }
        let name = target.unwrap_or_default().to_string();
        // The handle is taken and the map released before awaiting: a listing must
        // not hold the map across a peer's answer.
        let shell = {
            let mut shells = self
                .listing_shells
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            let set = shells
                .entry(name.clone())
                .or_insert_with(|| ShellSet::new(endpoint, None, self.shell_env.clone()));
            set.get(LISTING_SHELL)?
        };
        let command = listing_script(directory);
        let mut shell = shell.lock().await;
        // Generous on purpose: a peer behind a tunnel measured 11.7s for a warm
        // shell's first listing, and nothing here is on a keystroke -- the
        // front-end shows what has arrived and asks again on the next key.
        let outcome = shell
            .run(&command, std::time::Duration::from_secs(30), None)
            .await?;
        if outcome.exit_code != Some(0) {
            anyhow::bail!(
                "{} does not list {directory:?} on target {name:?}: {}",
                LISTING_SHELL,
                outcome.stderr.trim()
            );
        }
        let mut entries: Vec<(String, bool)> = outcome
            .stdout
            .lines()
            .filter_map(|line| match line.split_at_checked(2) {
                Some(("d ", rest)) if !rest.is_empty() => Some((rest.to_string(), true)),
                Some(("f ", rest)) if !rest.is_empty() => Some((rest.to_string(), false)),
                _ => None,
            })
            .collect();
        entries.sort();
        Ok(entries)
    }

    /// The target names a session may name, in the config's order.
    pub fn target_names(&self) -> Vec<String> {
        self.targets.iter().map(|t| t.name.clone()).collect()
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

    /// The model a session's row already carries, in the roster's spelling.
    ///
    /// `None` when the session does not exist yet or its row names no model: that
    /// is what makes `set_session` ask for `--model` instead of inventing one.
    pub fn recorded_model(&self, name: &str) -> Option<String> {
        let row = self.store.get_session(name).ok().flatten()?;
        let model = row.model.trim();
        if model.is_empty() {
            return None;
        }
        Some(
            crate::providers::roster()
                .place(model)
                .map(|e| e.model)
                .unwrap_or_else(|| model.to_string()),
        )
    }

    /// Creates a session or returns the existing one, refusing a target it does not have.
    ///
    /// # Invariant
    ///
    /// A session's target is written once, when it is created, and never changed:
    /// its shells may already exist on that peer, a lazily started remote shell has
    /// no pid to compare, and retired shells outlive their names -- so "has it
    /// started" cannot be answered reliably enough to permit a move. See
    /// `docs/notes/server-remote.md`.
    pub fn ensure_on(
        &self,
        name: &str,
        cwd: Option<&str>,
        model: Option<&str>,
        target: Option<&str>,
    ) -> anyhow::Result<SessionHandle> {
        // Resolved before anything is created: an unknown name must not make a session.
        let endpoint = self.endpoint_for(target)?;
        let recorded = target
            .map(str::trim)
            .filter(|t| !t.is_empty() && *t != "local");
        // Hold the live lock through the build: competing callers must not each
        // materialise and then overwrite one name.
        let mut live = self.live();
        if let Some(h) = live.get(name) {
            self.check_same_target(name, recorded)?;
            return Ok(h.clone());
        }
        if let Some(row) = self.store.get_session(name)? {
            self.check_same_target(name, recorded)?;
            let _ = endpoint;
            return self.materialise_locked(row, &mut live);
        }
        if matches!(endpoint, Endpoint::Local { .. }) && recorded.is_some() {
            anyhow::bail!("`local` is this machine, which is what naming no target means");
        }
        // A remote session with no directory starts where its endpoint is, not here.
        let cwd = match cwd {
            Some(path) => Some(PathBuf::from(path)),
            None if endpoint.is_remote() => None,
            None => Some(std::env::current_dir()?),
        };
        let stored = cwd
            .as_ref()
            .map(|path| path.to_string_lossy().into_owned())
            .unwrap_or_default();
        let source = model.unwrap_or(&self.model);
        let id = self
            .store
            .create_session_on(name, source, &stored, recorded)?;
        // Pin the model itself: reasoning blobs replay only where they were produced.
        let pinned = crate::providers::roster().place(source).map(|e| e.model);
        if let Some(pinned) = &pinned {
            let _ = self
                .store
                .set_session_tuning(id, Some(pinned), None, None, pinned);
        }
        self.materialise_locked(
            SessionRow {
                id,
                name: name.to_string(),
                status: protocol::SessionStatus::Idle,
                model: pinned.unwrap_or_else(|| source.to_string()),
                context_limit: None,
                thinking_effort: None,
                cwd: stored,
                created_at: 0,
                updated_at: 0,
                messages: 0,
                // A session that just came into existence has measured nothing.
                usage: protocol::Usage::default(),
                target: recorded.map(str::to_string),
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

    /// The endpoint a session's `target` names; `None` is this machine.
    ///
    /// # Invariant
    ///
    /// An unknown name is refused here: a session that asked for a peer must never
    /// quietly get a local shell instead. See `docs/notes/server-remote.md`.
    pub fn endpoint_for(&self, target: Option<&str>) -> anyhow::Result<Endpoint> {
        let Some(name) = target.map(str::trim).filter(|n| !n.is_empty()) else {
            return Ok(self.local_endpoint()?);
        };
        if name == "local" {
            return Ok(self.local_endpoint()?);
        }
        match self.targets.iter().find(|t| t.name == name) {
            Some(target) => Ok(target.endpoint.clone()),
            None => anyhow::bail!(
                "no target named {name:?}; this server knows {}",
                if self.targets.is_empty() {
                    "no targets at all".to_string()
                } else {
                    self.targets
                        .iter()
                        .map(|t| format!("{:?}", t.name))
                        .collect::<Vec<_>>()
                        .join(", ")
                }
            ),
        }
    }

    fn local_endpoint(&self) -> anyhow::Result<Endpoint> {
        let program = self
            .bash
            .program
            .clone()
            .ok_or_else(|| anyhow::anyhow!("no bash available; set [tools.bash] program"))?;
        Ok(Endpoint::local(program))
    }

    /// Refuses a target that differs from the one a session was created with.
    fn check_same_target(&self, name: &str, asked: Option<&str>) -> anyhow::Result<()> {
        let Some(asked) = asked else {
            return Ok(());
        };
        let recorded = self
            .store
            .get_session(name)?
            .and_then(|row| row.target)
            .unwrap_or_else(|| "local".to_string());
        if recorded != asked {
            anyhow::bail!(
                "session {name:?} runs on target {recorded:?}, and a target is fixed when the \
                 session is created: its shells are already on that machine. Create another \
                 session to run on {asked:?}."
            );
        }
        Ok(())
    }

    /// What a session's shells run on, as its row records it.
    pub fn recorded_target(&self, name: &str) -> Option<String> {
        self.store
            .get_session(name)
            .ok()
            .flatten()
            .and_then(|row| row.target)
    }

    fn materialise_locked(
        &self,
        row: SessionRow,
        live: &mut HashMap<String, SessionHandle>,
    ) -> anyhow::Result<SessionHandle> {
        if let Some(h) = live.get(&row.name) {
            return Ok(h.clone());
        }
        let endpoint = self.endpoint_for(row.target.as_deref())?;
        let cwd = (!row.cwd.trim().is_empty()).then(|| PathBuf::from(&row.cwd));
        // The endpoint judges its own directory: this filesystem would answer wrongly.
        let cwd = match (&endpoint, cwd) {
            (Endpoint::Local { .. }, Some(cwd)) if cwd.exists() => Some(cwd),
            (Endpoint::Local { .. }, _) => Some(std::env::current_dir()?),
            (_, cwd) => cwd,
        };
        let mut env = self.shell_env.clone();
        // A shell cannot learn its own session's name from the outside.
        env.push(("GONGGA_SESSION".to_string(), row.name.clone()));
        let mut shells = ShellSet::new(endpoint, cwd.clone(), env);
        // The default shell is part of the session's identity, so it exists
        // before anything asks for it.
        shells.get(DEFAULT_BASH)?;
        tracing::debug!(session = %row.name, model = %row.model, cwd = ?cwd, "session shells ready");
        let handle = SessionHandle {
            inner: Arc::new(SessionInner {
                id: row.id,
                name: row.name.clone(),
                store: self.store.clone(),
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
                closing: Arc::new(AtomicBool::new(false)),
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

/// The shell command that names each entry of `directory`, one per line.
///
/// # Invariant
///
/// `.[!.]*` is what keeps `.` and `..` out without a name test, and a name is
/// printed from `printf '%s'`, so a name with a space or a quote survives.
fn listing_script(directory: &str) -> String {
    let where_to = if directory.trim().is_empty() {
        ".".to_string()
    } else {
        quote_word(directory)
    };
    format!(
        "cd -- {where_to} 2>/dev/null || exit 3\n\
         for entry in .[!.]* *; do\n\
         \x20 [ -e \"$entry\" ] || continue\n\
         \x20 if [ -d \"$entry\" ]; then printf 'd %s\\n' \"$entry\"; \
         else printf 'f %s\\n' \"$entry\"; fi\n\
         done\n"
    )
}

/// One word for a POSIX shell: single-quoted, with an embedded quote escaped.
fn quote_word(word: &str) -> String {
    format!("'{}'", word.replace('\'', "'\\''"))
}
