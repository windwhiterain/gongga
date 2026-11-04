//! A row in SQLite plus the named shells that belong to it.
//! See `docs/notes/server-session.md`.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
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

use std::collections::HashMap as PythonMap;

/// The name of the shell every session starts with.
pub const DEFAULT_BASH: &str = "default";

/// Which machine a shell runs on.
///
/// # Invariant
///
/// The two are the whole of it: a session has this machine's shells always, and
/// its remote's only while it names a remote. See `docs/notes/server-remote.md`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Machine {
    Local,
    Remote,
}

impl Machine {
    /// How this machine is named in a sentence a reader acts on.
    pub fn named(self) -> &'static str {
        match self {
            Machine::Local => "this machine",
            Machine::Remote => "its remote",
        }
    }

    /// The label a listing carries, which is the word the tool is called by.
    pub fn label(self) -> &'static str {
        match self {
            Machine::Local => "local",
            Machine::Remote => "remote",
        }
    }
}

/// The shells one session owns: this machine's, and its remote's when it has one.
///
/// # Invariant
///
/// A name is one shell: a name used on one machine is refused on the other, never
/// quietly becoming a second process with the same handle. The directory belongs to
/// one machine -- the remote's when there is one -- and the other side starts in its
/// own default directory.
pub struct Shells {
    local: Arc<Mutex<ShellSet>>,
    /// The peer this session's directory is on, with the name it is known by.
    ///
    /// # Invariant
    ///
    /// The name travels with the set, never beside it: what the prompt's card says
    /// and what `remote_bash`'s `target` is answered with are one fact, and a second
    /// copy of it is a second answer waiting to disagree.
    remote: Option<Peer>,
}

/// One peer: its shells, and the name the machine's own file gave it.
struct Peer {
    name: String,
    set: Arc<Mutex<ShellSet>>,
}

impl Shells {
    /// The name of the peer this session's directory is on, when it is one.
    pub fn remote_name(&self) -> Option<&str> {
        self.remote.as_ref().map(|peer| peer.name.as_str())
    }

    /// The set one machine's shells live in, when the session has that machine.
    fn set(&self, machine: Machine) -> Option<&Arc<Mutex<ShellSet>>> {
        match machine {
            Machine::Local => Some(&self.local),
            Machine::Remote => self.remote.as_ref().map(|peer| &peer.set),
        }
    }

    /// The machine this session's directory belongs to.
    ///
    /// # Invariant
    ///
    /// One directory, one machine: it is the remote's when the session names one,
    /// and this machine's otherwise. Everything that reads a path -- the prompt's
    /// index, `@file:`, `@image:`, `@skill:` -- asks this, and nothing infers it
    /// from how a path is spelled.
    pub fn owner(&self) -> Machine {
        match self.remote {
            Some(_) => Machine::Remote,
            None => Machine::Local,
        }
    }

    pub fn has_remote(&self) -> bool {
        self.remote.is_some()
    }

    /// Every machine this session has shells on, `local` first.
    pub fn machines(&self) -> Vec<Machine> {
        match self.remote {
            Some(_) => vec![Machine::Local, Machine::Remote],
            None => vec![Machine::Local],
        }
    }

    /// Where one machine's shells run, for a caller that needs another program there.
    ///
    /// # Invariant
    ///
    /// `None` when the session has no shells on that machine, which is what a tool
    /// reports rather than starting the program on the wrong one.
    pub fn endpoint(&self, machine: Machine) -> Option<crate::remote::endpoint::Endpoint> {
        let set = self.set(machine)?;
        set.lock().ok().map(|set| set.endpoint().clone())
    }

    /// Where `machine`'s own `default` shell last said it was.
    pub fn reported_cwd(&self, machine: Machine) -> PathBuf {
        self.set(machine)
            .and_then(|set| set.lock().map(|set| set.reported_cwd()).ok())
            .unwrap_or_default()
    }

    /// Where this session stands: its own directory, on the machine that owns it.
    pub fn cwd(&self) -> PathBuf {
        self.reported_cwd(self.owner())
    }

    /// Whether `machine`'s shells are not this machine's.
    pub fn is_remote(&self, machine: Machine) -> bool {
        match machine {
            Machine::Local => false,
            Machine::Remote => self.remote.is_some(),
        }
    }

    /// The handle for `name` on `machine`, spawning it if this is its first use.
    pub fn get(
        &self,
        machine: Machine,
        name: &str,
    ) -> anyhow::Result<Arc<AsyncMutex<crate::remote::shell_set::Shell>>> {
        let Some(set) = self.set(machine) else {
            anyhow::bail!(
                "this session has no shells on {}: it names no remote",
                machine.named()
            );
        };
        // A name is one shell, whichever tool asked for it.
        if let Some(other) = self.other(machine)
            && let Ok(other) = other.lock()
            && other.peek(name).is_some()
        {
            anyhow::bail!(
                "`{name}` is already a shell on {}: a name is one shell, and `bash` and \
                 `remote_bash` share the names. Name this one otherwise, or \
                 `del_bash name_regex: \"^{name}$\"` first.",
                other_machine(machine).named()
            );
        }
        set.lock().unwrap_or_else(|e| e.into_inner()).get(name)
    }

    /// The handle without spawning.
    pub fn peek(
        &self,
        machine: Machine,
        name: &str,
    ) -> Option<Arc<AsyncMutex<crate::remote::shell_set::Shell>>> {
        self.set(machine)?
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .peek(name)
    }

    /// Where `name` lives, whichever machine holds it: `del_bash` and `check_bash`
    /// are asked for a shell, not for a machine.
    pub fn find(
        &self,
        name: &str,
    ) -> Option<(Machine, Arc<AsyncMutex<crate::remote::shell_set::Shell>>)> {
        for machine in self.machines() {
            if let Some(handle) = self.peek(machine, name) {
                return Some((machine, handle));
            }
        }
        None
    }

    /// Every shell this session has, as `(machine, name)`, sorted by both.
    pub fn names(&self) -> Vec<(Machine, String)> {
        let mut rows: Vec<(Machine, String)> = Vec::new();
        for machine in self.machines() {
            if let Some(set) = self.set(machine) {
                for name in set.lock().unwrap_or_else(|e| e.into_inner()).names() {
                    rows.push((machine, name));
                }
            }
        }
        rows
    }

    /// Puts a fresh shell under `name` in place of `handle`; false means another took it.
    pub fn replace(
        &self,
        machine: Machine,
        name: &str,
        handle: &Arc<AsyncMutex<crate::remote::shell_set::Shell>>,
    ) -> bool {
        let Some(set) = self.set(machine) else {
            return false;
        };
        match set
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .replace(name, handle)
        {
            Ok(replaced) => replaced,
            Err(e) => {
                tracing::warn!(shell = %name, error = %e, "could not put a fresh shell in place");
                false
            }
        }
    }

    /// Takes a shell and everything it left running, by name.
    pub fn discard(&self, name: &str) -> Vec<crate::remote::shell_set::Discarded> {
        let mut all = Vec::new();
        for machine in self.machines() {
            if let Some(set) = self.set(machine) {
                all.extend(
                    set.lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .discard_all(name),
                );
            }
        }
        all
    }

    /// Every shell, retired ones included, leaving the sets empty.
    pub fn drain(&self) -> Vec<crate::remote::shell_set::Discarded> {
        let mut all = Vec::new();
        for machine in self.machines() {
            if let Some(set) = self.set(machine) {
                let mut set = set.lock().unwrap_or_else(|e| e.into_inner());
                let drained = set.drain();
                let retired = set.take_retired();
                all.extend(drained);
                all.extend(retired);
            }
        }
        all
    }

    /// Drops `handle` from the retired list once its reader has finished.
    pub fn forget_retired(&self, handle: &Arc<AsyncMutex<crate::remote::shell_set::Shell>>) {
        for machine in self.machines() {
            if let Some(set) = self.set(machine) {
                set.lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .forget_retired(handle);
            }
        }
    }

    /// The arrangement a session is born with, and the one a move installs.
    ///
    /// # Invariant
    ///
    /// `cwd` belongs to the machine the session names: a remote session's directory
    /// is a path on that remote, and this machine's shells start wherever this
    /// machine starts by default. `default` is the directory-owning machine's, so
    /// the shell the model reaches for without a name is the one that is standing
    /// in the session's own directory.
    pub fn of(
        local: Endpoint,
        remote: Option<crate::remotes::RemoteConfig>,
        cwd: Option<PathBuf>,
        env: Vec<(String, String)>,
    ) -> anyhow::Result<Self> {
        // `cwd` belongs to the named machine; the other side starts where it starts.
        let here = match (&remote, &cwd) {
            (Some(_), _) => local_cwd()?,
            (None, Some(path)) => Some(path.clone()),
            (None, None) => local_cwd()?,
        };
        let mut here_set = ShellSet::new(local, here, env.clone());
        let there = match remote {
            Some(peer) => {
                let set = Arc::new(Mutex::new(ShellSet::new(peer.endpoint, cwd, env)));
                // `default` is the directory-owning machine's, and exists before any tool.
                set.lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .get(DEFAULT_BASH)?;
                Some(Peer {
                    name: peer.name,
                    set,
                })
            }
            None => None,
        };
        if there.is_none() {
            here_set.get(DEFAULT_BASH)?;
        }
        Ok(Self {
            local: Arc::new(Mutex::new(here_set)),
            remote: there,
        })
    }

    /// The other machine, when the session has one.
    fn other(&self, machine: Machine) -> Option<&Arc<Mutex<ShellSet>>> {
        match machine {
            Machine::Local => self.remote.as_ref().map(|peer| &peer.set),
            Machine::Remote => Some(&self.local),
        }
    }
}

/// Where this machine's shells start when the session's directory is elsewhere.
fn local_cwd() -> anyhow::Result<Option<PathBuf>> {
    Ok(Some(std::env::current_dir()?))
}

/// The machine on the other side of `machine`.
fn other_machine(machine: Machine) -> Machine {
    match machine {
        Machine::Local => Machine::Remote,
        Machine::Remote => Machine::Local,
    }
}

/// The shell a remote keeps only to answer directory listings for completion.
///
/// # Invariant
///
/// Named so it can never collide with a shell the model asks for: every tool
/// argument naming a shell is the model's own, and this one is not a session's.
const LISTING_SHELL: &str = "..completion";

/// What a `--remote` on the wire asked for.
///
/// # Invariant
///
/// `Keep` and `Machine(None)` are different instructions, and both are reachable
/// without a third spelling: omitting the flag keeps the machine, and `none` names
/// this one. A value that meant "nothing was said" must never be read as "come home".
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RemoteAsk {
    /// The flag was not given: stay where the row says.
    Keep,
    /// The flag was given: on this machine when it says `none`.
    Machine(Option<String>),
}

/// A `--remote` value as the machine it names: `none` is this machine.
pub fn remote_request(stated: &str) -> Option<String> {
    let name = stated.trim();
    if name.is_empty() || name.eq_ignore_ascii_case("none") {
        None
    } else {
        Some(name.to_string())
    }
}

/// Where a session was asked to move to, as `set_session` named it.
///
/// # Invariant
///
/// A remote of `None` is the machine the session already records, resolved when the
/// request is made: a request that meant "keep the machine" must not become "stay
/// wherever this field drifted to" by the time it runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MoveRequest {
    pub remote: Option<String>,
    pub cwd: Option<String>,
}

/// Why `name` may not be a session's, or `None` when it may.
///
/// # Invariant
///
/// A name is `/`-separated segments with none of them empty, because the list draws
/// one row per prefix: `a//b` and `a/` have no row to be drawn on, and a name nobody
/// can see is a name nobody can click. See `docs/notes/server-session.md`.
pub fn checked_name(name: &str) -> Option<String> {
    if name.is_empty() {
        return Some("a session name must not be empty".to_string());
    }
    let empty = name.split('/').any(str::is_empty);
    empty.then(|| {
        format!(
            "`{name}` has an empty segment: a name is `/`-separated segments \
             (`a/b/c`), with none of them empty"
        )
    })
}

/// What asking for a move came to.
#[derive(Debug)]
pub enum MoveOutcome {
    /// It was made now, and this is what it cost.
    Applied(MoveReport),
    /// A turn held the session, so it waits for that turn to end.
    Queued(MoveRequest),
}

/// What ending a session's shells came to, so a move can say it.
#[derive(Debug, Default)]
pub struct MoveReport {
    /// How many shells were ended, retired ones included.
    pub ended: usize,
    /// The ones whose end could not be proved; silence is not stopped.
    pub unconfirmed: Vec<crate::remote::transport::CloseReport>,
}

/// What a session was told to use, as `set_session` left it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Tuning {
    /// The model this session runs on, `provider/model_name`. `None` means "the
    /// server's own".
    pub model: Option<String>,
    /// The context level a notice is sent at, as a percentage of the window;
    /// `None` means the default share.
    pub context_limit: Option<u8>,
    /// How hard to think, in the roster's spelling; `None` is a model publishing no level.
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
    /// This machine's shells, and the remote's when the session names one.
    shells: Arc<Mutex<Shells>>,
    /// The persistent Python interpreters, named like shells.
    pythons: Arc<AsyncMutex<PythonMap<String, Arc<crate::tools::interpreter::Interpreter>>>>,
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
    /// Set by `cancel`: stop at the next storable point. See `request_stop`.
    stopping: AtomicBool,
    /// Set before the shells end, so a stop announced now is the command's, not the exit's.
    closing: Arc<AtomicBool>,
    /// The connection this session's references are read through, if any.
    reader: Mutex<Option<Arc<crate::remote::Reader>>>,
    /// The directory this session was placed at, as `set_session` last said.
    ///
    /// # Invariant
    ///
    /// Not where its shell stands: a `cd` moves a shell and not a session, so this is
    /// what the prompt's index is read against. Only a move changes it, and a session
    /// on a remote it has not reached yet has none -- the first connection fills it.
    /// See `docs/notes/server-meta.md`.
    placed: Mutex<Option<PathBuf>>,
    /// A move asked for while a turn held the session, waiting for that turn to end.
    ///
    /// # Invariant
    ///
    /// Only the holder of the gate may take it: applying a move ends shells, and the
    /// turn that held the gate read the shell set at its start.
    pending_move: Mutex<Option<MoveRequest>>,
}

impl SessionHandle {
    pub fn id(&self) -> i64 {
        self.inner.id
    }

    pub fn name(&self) -> &str {
        &self.inner.name
    }

    pub fn shells(&self) -> Arc<Mutex<Shells>> {
        self.inner.shells.clone()
    }

    pub fn pythons(
        &self,
    ) -> Arc<AsyncMutex<PythonMap<String, Arc<crate::tools::interpreter::Interpreter>>>> {
        self.inner.pythons.clone()
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
        let discarded = {
            let shells = self.inner.shells.lock().unwrap_or_else(|e| e.into_inner());
            shells.drain()
        };
        tracing::info!(count = discarded.len(), "closing shells");
        for discarded in discarded {
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

    /// Ends every shell this session has, without claiming it is gone for good.
    ///
    /// # Invariant
    ///
    /// Never sets `closing`: that flag tells a tool call its command was stopped by
    /// the server's own exit, and a move is not an exit. What a shell's end could
    /// not prove is reported to the caller, which is the one that can say so.
    async fn end_shells(&self) -> MoveReport {
        let discarded = {
            let shells = self.inner.shells.lock().unwrap_or_else(|e| e.into_inner());
            shells.drain()
        };
        let ended = discarded.len();
        let mut unconfirmed = Vec::new();
        for discarded in discarded {
            let report = discarded.end().await;
            if !report.confirmed {
                unconfirmed.push(report);
            }
        }
        MoveReport { ended, unconfirmed }
    }

    /// Puts this session's shells on another endpoint, in another directory.
    ///
    /// # Invariant
    ///
    /// The caller holds the gate: a turn reads the shell set at its start and holds
    /// it for the turn, so a move must not land between those two facts. Every shell
    /// is ended before the new set exists, and the swap is one assignment under the
    /// lock every tool call reads it through.
    pub async fn relocate(
        &self,
        local: Endpoint,
        remote: Option<crate::remotes::RemoteConfig>,
        cwd: Option<PathBuf>,
        env: Vec<(String, String)>,
    ) -> anyhow::Result<MoveReport> {
        let report = self.end_shells().await;
        let fresh = Shells::of(local, remote, cwd, env)?;
        {
            let mut shells = self.inner.shells.lock().unwrap_or_else(|e| e.into_inner());
            *shells = fresh;
        }
        tracing::info!(
            session = %self.inner.name,
            ended = report.ended,
            unconfirmed = report.unconfirmed.len(),
            "session moved"
        );
        Ok(report)
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
        // A quarter of the budget is the granule: told again before it doubles, not every
        // step.
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
        // A new turn is not a stopping one: a shutdown that asked the last turn to
        // stop has no claim on this one.
        self.inner.stopping.store(false, Ordering::SeqCst);
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

    /// Asks the running turn to stop where it can be stored whole.
    ///
    /// # Invariant
    ///
    /// The token is left alone: a tool killed mid-flight would be stored as a
    /// sentence about being stopped. See `docs/notes/server-session.md`.
    pub fn request_stop(&self) {
        self.inner.resumable.store(true, Ordering::SeqCst);
        self.inner.stopping.store(true, Ordering::SeqCst);
        self.inner.cancels.fetch_add(1, Ordering::SeqCst);
    }

    /// Whether a shutdown asked this turn to stop at its next boundary.
    pub fn stop_requested(&self) -> bool {
        self.inner.stopping.load(Ordering::SeqCst)
    }

    /// Remembers a move for whoever holds this session next.
    pub fn queue_move(&self, request: MoveRequest) {
        *self
            .inner
            .pending_move
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = Some(request);
    }

    /// Takes the queued move, if there is one.
    pub fn take_move(&self) -> Option<MoveRequest> {
        self.inner
            .pending_move
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take()
    }

    /// Whether a move is waiting; read by a turn that ends, before it lets go.
    pub fn has_move(&self) -> bool {
        self.inner
            .pending_move
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .is_some()
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
            .cwd()
    }

    /// The connection this session's references are read through, when its
    /// directory is on a remote.
    pub fn reader(&self) -> Option<Arc<crate::remote::Reader>> {
        self.inner
            .reader
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// Points the reading side at the machine the session now stands on.
    pub fn set_reader(&self, reader: Option<Arc<crate::remote::Reader>>) {
        *self.inner.reader.lock().unwrap_or_else(|e| e.into_inner()) = reader;
    }

    /// The directory this session was placed at, when one has been named.
    pub fn placed(&self) -> Option<PathBuf> {
        self.inner
            .placed
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// Records the directory a move put this session in.
    pub fn set_placed(&self, placed: Option<PathBuf>) {
        *self.inner.placed.lock().unwrap_or_else(|e| e.into_inner()) = placed;
    }

    /// Fills the placement once, when the endpoint's first shell reports it.
    ///
    /// # Invariant
    ///
    /// Never overwrites: a placement is a statement and a report is where a shell
    /// happens to stand, so only the absence of one is filled.
    pub fn place_if_unstated(&self, directory: &std::path::Path) {
        if directory.as_os_str().is_empty() {
            return;
        }
        let mut placed = self.inner.placed.lock().unwrap_or_else(|e| e.into_inner());
        if placed.is_none() {
            *placed = Some(directory.to_path_buf());
        }
    }

    /// The machine this session's directory belongs to.
    pub fn owner(&self) -> Machine {
        self.shells()
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .owner()
    }

    /// The peer this session's directory is on, when it is not this machine.
    ///
    /// # Invariant
    ///
    /// Read from the live shells, not from the row: the row is what a restart hands
    /// back, and a move swaps the shells under the lock every tool reads them through.
    pub fn remote_name(&self) -> Option<String> {
        self.shells()
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remote_name()
            .map(str::to_string)
    }

    /// Whether this session's shells run somewhere other than this machine.
    pub fn on_remote(&self) -> bool {
        self.shells()
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .has_remote()
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
        // Only the remote's report is a fact about the row.
        let cwd = {
            let shells = self.inner.shells.lock().unwrap_or_else(|e| e.into_inner());
            if shells.owner() != Machine::Remote {
                return;
            }
            shells.reported_cwd(Machine::Remote)
        };
        if cwd.as_os_str().is_empty() {
            return;
        }
        // The first report is where this session was put; a later `cd` is not.
        self.place_if_unstated(&cwd);
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

/// What a stated directory means for a shell on this machine.
///
/// # Invariant
///
/// Absolute, `~` expanded, and read from `base` when relative: the row is what a
/// later shell is started in, so a path stored as typed would move with it.
pub fn stated_directory(stated: &str, base: &Path) -> PathBuf {
    let trimmed = stated.trim();
    let expanded = match trimmed.strip_prefix('~') {
        Some(rest) if rest.is_empty() || rest.starts_with(['/', '\\']) => match dirs::home_dir() {
            Some(home) => home.join(rest.trim_start_matches(['/', '\\'])),
            None => PathBuf::from(trimmed),
        },
        _ => PathBuf::from(trimmed),
    };
    let joined = base.join(expanded);
    std::path::absolute(&joined).unwrap_or(joined)
}

/// Where a stated `--cwd` lands, resolved on the destination's side.
///
/// # Invariant
///
/// A path landing on this machine is made absolute against `base`; a path
/// landing on a peer is passed through as written, because only that machine
/// can expand it.
pub fn placed_directory(stated: &str, remote: Option<&str>, base: &Path) -> PathBuf {
    match remote {
        None => stated_directory(stated, base),
        Some(_) => PathBuf::from(stated.trim()),
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
    remotes: Vec<crate::remotes::RemoteConfig>,
    live: Arc<Mutex<HashMap<String, SessionHandle>>>,
    /// One shell per remote, opened only to list a directory for completion.
    ///
    /// # Invariant
    ///
    /// Kept apart from every session's shells and never locked while `live` is:
    /// a directory listing is answered while a turn is running, and a Tab must not
    /// queue behind one. See `docs/notes/server-remote.md`.
    listing_shells: Arc<Mutex<HashMap<String, ShellSet>>>,
    /// One file-reading connection per remote, and nobody's shell.
    ///
    /// # Invariant
    ///
    /// Not a session's shell: a message is delivered while that session's command may
    /// be running, and waiting for its lock would make an instant inject wait for a
    /// build. Keyed by remote, so two sessions on one peer share one connection.
    readers: Arc<Mutex<HashMap<String, Arc<crate::remote::Reader>>>>,
}

impl SessionManager {
    pub fn new(
        store: Store,
        bash: BashConfig,
        model: String,
        detached: DetachedRegistry,
        shell_env: Vec<(String, String)>,
        remotes: Vec<crate::remotes::RemoteConfig>,
    ) -> Self {
        Self {
            store,
            bash,
            model,
            detached,
            shell_env,
            remotes,
            live: Arc::new(Mutex::new(HashMap::new())),
            listing_shells: Arc::new(Mutex::new(HashMap::new())),
            readers: Arc::new(Mutex::new(HashMap::new())),
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
        remote: Option<&str>,
        directory: &str,
    ) -> anyhow::Result<Vec<(String, bool)>> {
        let endpoint = self.endpoint_for(remote)?;
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
        let name = remote.unwrap_or_default().to_string();
        let outcome = match self.listing_run(&name, endpoint, directory).await {
            Ok(outcome) => outcome,
            // A dead handle never works again, and a shell the peer lost is not a
            // directory it refused.
            Err(_) => {
                self.listing_run(&name, self.endpoint_for(remote)?, directory)
                    .await?
            }
        };
        // No status means the shell died: waiting has no deadline, so a listing either
        // reports one or did not answer.
        let Some(status) = outcome.exit_code else {
            anyhow::bail!(
                "remote {name:?} did not answer while listing {directory:?}, and its shell \
                 never reported a status"
            );
        };
        if status != 0 {
            anyhow::bail!(
                "remote {name:?} cannot list {directory:?}: {}",
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

    /// One listing through the shell kept for `name`, dropping it if that fails.
    ///
    /// # Invariant
    ///
    /// A run that fails forgets the shell: a peer that has lost one of its own
    /// answers every later run with a refusal, and the kept handle would turn one
    /// bad minute into a remote whose completion never works again.
    async fn listing_run(
        &self,
        name: &str,
        endpoint: Endpoint,
        directory: &str,
    ) -> anyhow::Result<crate::tools::bash::BashOutcome> {
        // The handle is taken and the map released before awaiting: a listing must
        // not hold the map across a peer's answer.
        let shell = {
            let mut shells = self
                .listing_shells
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            let set = shells
                .entry(name.to_string())
                .or_insert_with(|| ShellSet::new(endpoint, None, self.shell_env.clone()));
            set.get(LISTING_SHELL)?
        };
        let mut shell = shell.lock().await;
        // A path typed for this machine is not always one its shell reads.
        let directory = match shell.canonical(directory).await {
            Ok(canonical) => canonical,
            // A directory the peer cannot name is one its listing will refuse, with
            // a message about the path the caller wrote.
            Err(_) => directory.to_string(),
        };
        let command = listing_script(&directory);
        // Asked and waited for: no deadline, a cold `System32` listing measured 6.6s.
        let outcome = shell.run_until_settled(&command).await;
        if outcome.is_err() {
            drop(shell);
            self.forget_listing_shell(name);
        }
        outcome
    }

    /// Refuses a directory the destination does not have.
    ///
    /// # Invariant
    ///
    /// Asked of a `--cwd` a caller states now, never of one a row records: a
    /// session whose directory has since been removed still starts where its row
    /// says, so the removal refuses that command rather than that session.
    pub async fn check_placement(
        &self,
        remote: Option<&str>,
        directory: &Path,
    ) -> anyhow::Result<()> {
        let endpoint = self.endpoint_for(remote)?;
        if !endpoint.is_remote() {
            if directory.is_dir() {
                return Ok(());
            }
            let fault = if directory.exists() {
                "is not a directory"
            } else {
                "does not exist"
            };
            anyhow::bail!("--cwd {} {fault} on this machine", directory.display());
        }
        let name = remote.unwrap_or_default();
        let reader = self
            .reader_for(Some(name))
            .ok_or_else(|| anyhow::anyhow!("remote {name:?} has no readable filesystem"))?;
        let path = directory.to_string_lossy();
        let file = reader.stat(&path).await?;
        if file.is_dir {
            return Ok(());
        }
        let fault = if file.exists {
            "is not a directory"
        } else {
            "does not exist"
        };
        anyhow::bail!("--cwd {path} {fault} on remote {name:?}")
    }

    /// The connection a session on `remote` reads its files through.
    ///
    /// # Invariant
    ///
    /// One per remote, and `None` when the session stands on this machine: a local
    /// file is read by the process that is already able to.
    fn reader_for(&self, remote: Option<&str>) -> Option<Arc<crate::remote::Reader>> {
        let name = remote.map(str::trim).filter(|n| !n.is_empty())?;
        if let Some(reader) = self
            .readers
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(name)
        {
            return Some(reader.clone());
        }
        let endpoint = self.endpoint_for(Some(name)).ok()?;
        if !endpoint.is_remote() {
            return None;
        }
        let reader = Arc::new(crate::remote::Reader::new(endpoint));
        self.readers
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(name.to_string(), reader.clone());
        Some(reader)
    }

    /// Forgets a remote's listing shell, so the next question opens a fresh one.
    ///
    /// # Invariant
    ///
    /// Called with nothing in flight: a shell whose command is still running cannot
    /// be replaced without its lock, which is exactly what a stall holds.
    fn forget_listing_shell(&self, remote: &str) {
        let mut shells = self
            .listing_shells
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        shells.remove(remote);
    }

    /// The remote names a session may name, in the config's order.
    pub fn remote_names(&self) -> Vec<String> {
        self.remotes.iter().map(|r| r.name.clone()).collect()
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

    /// The model the row already carries; `None` is why `set_session` demands one.
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

    /// Creates a session or returns the existing one, refusing a remote it does not have.
    ///
    /// # Invariant
    ///
    /// Never moves an existing session: a move ends shells, and only
    /// [`SessionManager::relocate`] may do that, under the gate. This refuses a
    /// different remote and names the command that does move one, so the two paths
    /// cannot disagree about what "the same session" means. See
    /// `docs/notes/server-remote.md`.
    pub fn ensure_on(
        &self,
        name: &str,
        cwd: Option<&str>,
        model: Option<&str>,
        remote: Option<&str>,
    ) -> anyhow::Result<SessionHandle> {
        // Resolved before anything is created: an unknown name must not make a session.
        let recorded = remote.and_then(remote_request);
        let endpoint = self.endpoint_for(recorded.as_deref())?;
        // Hold the live lock through the build: competing callers must not each
        // materialise and then overwrite one name.
        let mut live = self.live();
        if let Some(h) = live.get(name) {
            self.check_same_remote(name, recorded.as_deref())?;
            return Ok(h.clone());
        }
        if let Some(row) = self.store.get_session(name)? {
            self.check_same_remote(name, recorded.as_deref())?;
            let _ = endpoint;
            return self.materialise_locked(row, &mut live);
        }
        // A remote session with no directory starts where its endpoint is. A
        // stated one is read where its shell would read it.
        let cwd = match (cwd, endpoint.is_remote()) {
            (Some(stated), remote) => Some(placed_directory(
                stated,
                if remote { recorded.as_deref() } else { None },
                &std::env::current_dir()?,
            )),
            (None, true) => None,
            (None, false) => Some(std::env::current_dir()?),
        };
        let stored = cwd
            .as_ref()
            .map(|path| path.to_string_lossy().into_owned())
            .unwrap_or_default();
        let source = model.unwrap_or(&self.model);
        let id = self
            .store
            .create_session_on(name, source, &stored, recorded.as_deref())?;
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
                remote: recorded.clone(),
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

    /// The endpoint a session's `remote` names; `None` is this machine.
    ///
    /// # Invariant
    ///
    /// An unknown name is refused here: a session that asked for a peer must never
    /// quietly get a local shell instead. See `docs/notes/server-remote.md`.
    /// The peer a name identifies, as the machine's own file spelled it.
    ///
    /// # Invariant
    ///
    /// The name travels with the endpoint: what the card says, what `target` is
    /// answered with and where the shells run are one fact. A name for this machine
    /// (`none`, or nothing) is `None` -- never a peer called "none".
    pub fn peer(
        &self,
        remote: Option<&str>,
    ) -> anyhow::Result<Option<crate::remotes::RemoteConfig>> {
        let Some(name) = remote.map(str::trim).filter(|n| !n.is_empty()) else {
            return Ok(None);
        };
        let endpoint = self.endpoint_for(Some(name))?;
        Ok(endpoint.is_remote().then(|| crate::remotes::RemoteConfig {
            name: name.to_string(),
            endpoint,
        }))
    }

    pub fn endpoint_for(&self, remote: Option<&str>) -> anyhow::Result<Endpoint> {
        let Some(name) = remote.map(str::trim).filter(|n| !n.is_empty()) else {
            return Ok(self.local_endpoint()?);
        };
        if name.eq_ignore_ascii_case("none") {
            return Ok(self.local_endpoint()?);
        }
        if name == "local" {
            anyhow::bail!(
                "there is no remote called `local`: name `none`, or leave `--remote` out, \
                 for this machine"
            );
        }
        match self.remotes.iter().find(|r| r.name == name) {
            Some(remote) => Ok(remote.endpoint.clone()),
            None => anyhow::bail!(
                "no remote named {name:?}; this server knows {}",
                if self.remotes.is_empty() {
                    "no remotes at all".to_string()
                } else {
                    self.remotes
                        .iter()
                        .map(|t| format!("{:?}", t.name))
                        .collect::<Vec<_>>()
                        .join(", ")
                }
            ),
        }
    }

    /// The variables every shell of `name` runs with.
    fn session_env(&self, name: &str) -> Vec<(String, String)> {
        let mut env = self.shell_env.clone();
        // A shell cannot learn its own session's name from the outside.
        env.push(("GONGGA_SESSION".to_string(), name.to_string()));
        // ... nor its model or machine: a child is placed from these,
        // not a flagless `set_session`; sticky fill would move it.
        if let Some(row) = self.store.get_session(name).ok().flatten() {
            if !row.model.trim().is_empty() {
                env.push(("GONGGA_MODEL".to_string(), row.model.clone()));
            }
            if let Some(remote) = row.remote.filter(|remote| !remote.trim().is_empty()) {
                env.push(("GONGGA_REMOTE".to_string(), remote));
            }
        }
        env
    }

    fn local_endpoint(&self) -> anyhow::Result<Endpoint> {
        let program = self
            .bash
            .program
            .clone()
            .ok_or_else(|| anyhow::anyhow!("no bash available; set [tools.bash] program"))?;
        Ok(Endpoint::local(program))
    }

    /// Puts an existing session's shells on `remote`, in `cwd`.
    ///
    /// # Invariant
    ///
    /// Never applied under a running turn, always remembered instead. The turn holds
    /// the gate, read the shell set at its start and keeps it, so a move landing
    /// under it would end shells a tool call is using. Queueing rather than refusing
    /// is what makes the feature reachable at all: the one caller that wants to move
    /// a session is the agent working inside it, which is always mid-turn.
    pub async fn relocate(
        &self,
        session: &SessionHandle,
        ask: RemoteAsk,
        cwd: Option<&str>,
    ) -> anyhow::Result<MoveOutcome> {
        // An unstated remote is the one it records: a directory move must not move machines.
        let wanted_remote = match ask {
            RemoteAsk::Keep => self.recorded_remote(session.name()),
            RemoteAsk::Machine(remote) => remote,
        };
        // Resolved before anything is queued: an unknown name must not tear down shells.
        self.endpoint_for(wanted_remote.as_deref())?;
        let request = MoveRequest {
            remote: wanted_remote,
            cwd: cwd.map(str::to_string),
        };
        let Some(_gate) = session.try_claim() else {
            session.queue_move(request.clone());
            tracing::info!(
                session = %session.name(),
                remote = ?request.remote,
                "move queued behind the running turn"
            );
            return Ok(MoveOutcome::Queued(request));
        };
        let report = self.apply_move(session, &request).await?;
        Ok(MoveOutcome::Applied(report))
    }

    /// Makes a queued move real, for whoever is holding the session.
    ///
    /// # Invariant
    ///
    /// The caller holds the gate. Returns the report when there was something to do,
    /// and `None` when nothing was waiting -- so the same call at the end of every
    /// turn costs a lock and nothing else.
    pub async fn apply_queued_move(
        &self,
        session: &SessionHandle,
    ) -> Option<anyhow::Result<MoveReport>> {
        let request = session.take_move()?;
        Some(self.apply_move(session, &request).await)
    }

    /// Ends the shells and records the new placement. The caller holds the gate.
    async fn apply_move(
        &self,
        session: &SessionHandle,
        request: &MoveRequest,
    ) -> anyhow::Result<MoveReport> {
        let remote = self.peer(request.remote.as_deref())?;
        let single_cwd = remote.is_none();
        // The row is handed to a shell later, and a relative path there would
        // mean wherever that shell happens to stand.
        let wanted = match request.cwd.as_deref() {
            Some(stated) => Some(placed_directory(
                stated,
                if single_cwd {
                    None
                } else {
                    request.remote.as_deref()
                },
                &session.cwd(),
            )),
            None => None,
        };
        let env = self.session_env(session.name());
        let report = session
            .relocate(self.local_endpoint()?, remote, wanted.clone(), env)
            .await?;
        // The reading side moves with the shells.
        session.set_reader(self.reader_for(request.remote.as_deref()));
        // A move states the placement; a shell's report does not.
        session.set_placed(wanted.clone());
        // The row is what the next start hands back to the endpoint, so it moves
        // with the shells rather than after them.
        let placed = match (&wanted, session.cwd().to_string_lossy().to_string()) {
            (Some(path), _) => path.to_string_lossy().into_owned(),
            (None, reported) if !reported.is_empty() => reported,
            (None, _) => String::new(),
        };
        self.store
            .set_session_placement(session.name(), &placed, request.remote.as_deref())?;
        Ok(report)
    }

    /// Refuses a remote that differs from the one a session was created with.
    fn check_same_remote(&self, name: &str, asked: Option<&str>) -> anyhow::Result<()> {
        let Some(asked) = asked else {
            return Ok(());
        };
        let recorded = self
            .store
            .get_session(name)?
            .and_then(|row| row.remote)
            .unwrap_or_else(|| "this machine".to_string());
        if recorded != asked {
            anyhow::bail!(
                "session {name:?} runs on remote {recorded:?}, not {asked:?}; moving it \
                 ends the shells on the machine it is leaving, so it is `set_session \
                 --remote` that does that, not creating the session."
            );
        }
        Ok(())
    }

    /// What a session's shells run on, as its row records it.
    pub fn recorded_remote(&self, name: &str) -> Option<String> {
        self.store
            .get_session(name)
            .ok()
            .flatten()
            .and_then(|row| row.remote)
    }

    fn materialise_locked(
        &self,
        row: SessionRow,
        live: &mut HashMap<String, SessionHandle>,
    ) -> anyhow::Result<SessionHandle> {
        if let Some(h) = live.get(&row.name) {
            return Ok(h.clone());
        }
        // The row's own spelling, kept for the placement below: `row.cwd` moves on.
        let row_cwd = row.cwd.clone();
        let remote = self.peer(row.remote.as_deref())?;
        let cwd = (!row.cwd.trim().is_empty()).then(|| PathBuf::from(&row.cwd));
        // The endpoint judges its own directory: this filesystem would answer wrongly.
        let cwd = match (&remote, cwd) {
            (None, Some(cwd)) if cwd.exists() => Some(cwd),
            (None, _) => Some(std::env::current_dir()?),
            (Some(_), cwd) => cwd,
        };
        let env = self.session_env(&row.name);
        let shells = Shells::of(self.local_endpoint()?, remote, cwd.clone(), env)?;
        let reader = self.reader_for(row.remote.as_deref());
        tracing::debug!(session = %row.name, model = %row.model, cwd = ?cwd, "session shells ready");
        let handle = SessionHandle {
            inner: Arc::new(SessionInner {
                id: row.id,
                name: row.name.clone(),
                store: self.store.clone(),
                shells: Arc::new(Mutex::new(shells)),
                pythons: Arc::new(AsyncMutex::new(PythonMap::new())),
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
                stopping: AtomicBool::new(false),
                closing: Arc::new(AtomicBool::new(false)),
                reader: Mutex::new(reader),
                placed: Mutex::new((!row_cwd.trim().is_empty()).then(|| PathBuf::from(&row_cwd))),
                pending_move: Mutex::new(None),
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
