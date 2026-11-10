//! A row in SQLite plus the named workspace that belong to it.
//! See `docs/notes/server-session.md`.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

use tokio::sync::Mutex as AsyncMutex;
use tokio::sync::OwnedMutexGuard;
use tokio_util::sync::CancellationToken;

use crate::config::BashConfig;
use crate::remote::endpoint::Endpoint;
use crate::store::{SessionRow, Store};
use crate::tools::detached::DetachedRegistry;

/// The name of the shell every session starts with.
pub const DEFAULT_BASH: &str = "default";

/// Which machine a shell runs on.
///
/// # Invariant
///
/// The two are the whole of it: a session has this machine's workspace always, and
/// its remote's only while it names a remote. See `docs/notes/server-remote.md`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Machine {
    Local,
    Remote,
}

/// Where a session's work happens: its directory, and the machine that holds it.
///
/// # Invariant
///
/// One directory, one machine: the prompt's index, `@file(...)`, `@image(...)`,
/// `@skill(...)` and a channel's default `cwd` all ask this, and nothing infers it
/// from how a path is spelled. The directory is the session's own -- a `cd` typed in
/// a channel belongs to that channel, and never moves the session.
pub struct Workspace {
    local: Endpoint,
    /// The peer that holds this session's directory, with the name it is known by.
    remote: Option<Peer>,
    /// The session's directory: a path on the machine that owns it.
    cwd: PathBuf,
}

/// One peer: its endpoint, and the name the machine's own file gave it.
struct Peer {
    name: String,
    endpoint: Endpoint,
}

impl Workspace {
    /// A workspace with no peer: what one call in its own process gets.
    pub fn empty(program: std::path::PathBuf) -> Self {
        Self {
            local: Endpoint::local(program),
            remote: None,
            cwd: PathBuf::new(),
        }
    }

    /// The name of the peer this session's directory is on, when it is one.
    pub fn remote_name(&self) -> Option<&str> {
        self.remote.as_ref().map(|peer| peer.name.as_str())
    }

    /// The endpoint of `machine`, when this session has that machine.
    pub fn endpoint(&self, machine: Machine) -> Option<Endpoint> {
        match machine {
            Machine::Local => Some(self.local.clone()),
            Machine::Remote => self.remote.as_ref().map(|peer| peer.endpoint.clone()),
        }
    }

    /// Where `machine` starts: the session's directory, empty on the other machine.
    pub fn dir(&self, machine: Machine) -> PathBuf {
        if machine == self.owner() {
            self.cwd.clone()
        } else {
            PathBuf::new()
        }
    }

    /// The machine this session's directory belongs to.
    pub fn owner(&self) -> Machine {
        if self.remote.is_some() {
            Machine::Remote
        } else {
            Machine::Local
        }
    }

    /// Where this session stands: its own directory, on the machine that owns it.
    pub fn cwd(&self) -> PathBuf {
        self.cwd.clone()
    }

    /// Whether this session's directory is on a peer at all.
    pub fn has_remote(&self) -> bool {
        self.remote.is_some()
    }

    /// Whether `machine`'s work is not this machine's.
    pub fn is_remote(&self, machine: Machine) -> bool {
        match machine {
            Machine::Local => false,
            Machine::Remote => self.remote.is_some(),
        }
    }

    /// The arrangement a session is born with, and the one a move installs.
    ///
    /// # Invariant
    ///
    /// `cwd` belongs to the machine the session names: a remote session's directory
    /// is a path on that remote, and a session with none starts where this machine
    /// starts by default.
    pub fn of(
        local: Endpoint,
        remote: Option<crate::remotes::RemoteConfig>,
        cwd: Option<PathBuf>,
    ) -> anyhow::Result<Self> {
        let remote = remote.map(|peer| Peer {
            name: peer.name,
            endpoint: peer.endpoint,
        });
        let cwd = match (&remote, cwd) {
            (Some(_), Some(path)) => path,
            (Some(_), None) => PathBuf::new(),
            (None, Some(path)) => path,
            (None, None) => std::env::current_dir()?,
        };
        Ok(Self { local, remote, cwd })
    }
}

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

/// What ending a session's workspace came to, so a move can say it.
#[derive(Debug, Default)]
pub struct MoveReport {
    /// How many workspace were ended, retired ones included.
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
    /// The session's static tier; `None` picks the shortest declared tier.
    pub cache_tier: Option<String>,
    /// Whether a periodic time notice keeps this session's prefix warm.
    pub cache_keepalive: bool,
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
    /// This machine's workspace, and the remote's when the session names one.
    workspace: Arc<Mutex<Workspace>>,
    /// What a program this session starts is given before its own rows. See `env`.
    script_env: Vec<(String, String)>,
    /// The persistent Python interpreters, named like workspace.
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
    /// The workspace look this session takes after each program call, when what it
    /// is for is a read. `None` is work mode.
    ///
    /// # Invariant
    ///
    /// Behind a lock because `set_session --mode` may land while a turn runs, and one
    /// value for both facts -- the mode in force is `is_some`. Every tool call reads
    /// it again, so the mode a call runs under is the one in force when it was made.
    /// See `docs/notes/server-explore.md`.
    explore: Mutex<Option<Arc<crate::explore::Watch>>>,
    /// Set once the rows are gone, so a queued turn gives up instead of writing.
    deleted: AtomicBool,
    /// Set by a `cancel` that found the turn in `wait`: the next start resumes it.
    resumable: Arc<AtomicBool>,
    /// Set by `interrupt_session`: the turn ends where it stands, and no restart owes it.
    ///
    /// # Invariant
    ///
    /// Cleared when the next turn starts, so it says what ended the turn in flight and
    /// nothing about the one after it. A tool call reads it to say which stop cut it,
    /// and the session row reads it to say the same to a window.
    interrupted: Arc<AtomicBool>,
    /// Whether a call is inside `wait` right now: the one work a cancel keeps.
    waiting: Arc<AtomicBool>,
    /// When the wait ends, epoch ms; 0 is none. A restart continues what was left.
    until: Arc<AtomicI64>,
    /// Set before the workspace end, so a stop announced now is the command's, not the exit's.
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
    /// The parts of an answer that are still being written.
    ///
    /// # Invariant
    ///
    /// What a refill sends besides the stored rows, and the only copy of the text
    /// after the last row: the drafts on disk lag by a quarter second, so a window
    /// that reconnected mid-answer would otherwise lose that tail. Every update lands
    /// inside [`crate::bus::EventBus::step`], so text and cursor are read together.
    open: Mutex<Vec<OpenPart>>,
    /// A move asked for while a turn held the session, waiting for that turn to end.
    ///
    /// # Invariant
    ///
    /// Only the holder of the gate may take it: applying a move ends workspace, and the
    /// turn that held the gate read the shell set at its start.
    pending_move: Mutex<Option<MoveRequest>>,
}

/// One part of an answer that has not been stored yet.
///
/// # Invariant
///
/// The text is the whole of what has been published for this part, and it is
/// only ever read inside the event bus's gate: a reader that took it outside
/// would have text whose chunks it was also about to be told, and the same
/// letters would land twice. See `docs/notes/server-session.md`.
#[derive(Debug, Clone)]
pub struct OpenPart {
    /// The provider's id for the item, so a row landing can retire its part.
    pub item: String,
    pub turn: u64,
    pub part: protocol::ResponsePart,
    pub call_id: Option<String>,
    pub text: String,
}

impl SessionHandle {
    /// Adds a chunk to the part being written, keeping the text so far.
    pub fn open_push(
        &self,
        item: &str,
        turn: u64,
        part: protocol::ResponsePart,
        call_id: Option<String>,
        text: &str,
    ) {
        let mut open = self.inner.open.lock().unwrap_or_else(|e| e.into_inner());
        match open.iter_mut().find(|open| open.item == item) {
            Some(entry) => entry.text.push_str(text),
            None => open.push(OpenPart {
                item: item.to_string(),
                turn,
                part,
                call_id,
                text: text.to_string(),
            }),
        }
    }

    /// Forgets one part: its row is written, so the transcript says it now.
    pub fn open_done(&self, item: &str) {
        let mut open = self.inner.open.lock().unwrap_or_else(|e| e.into_inner());
        open.retain(|open| open.item != item);
    }

    /// Forgets every part still being written: the turn that wrote them ended.
    pub fn open_clear(&self) {
        self.inner
            .open
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clear();
    }

    /// Every part still being written, as of this moment.
    pub fn open_parts(&self) -> Vec<OpenPart> {
        self.inner
            .open
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    pub fn id(&self) -> i64 {
        self.inner.id
    }

    pub fn name(&self) -> &str {
        &self.inner.name
    }

    pub fn workspace(&self) -> Arc<Mutex<Workspace>> {
        self.inner.workspace.clone()
    }

    /// What a program this session starts sees: never the server's own environment.
    pub fn env(&self) -> Vec<(String, String)> {
        let mut env = self.inner.script_env.clone();
        env.extend(session_vars(&self.inner.store, &self.inner.name));
        env
    }

    /// Marks the way out, so a call in flight can say what ended it.
    ///
    /// # Invariant
    ///
    /// A channel's program is a relay's, not this server's child, so nothing here
    /// ends one: `cancel` is what stops them. This flag is only what tells a running
    /// call that the stop was the server's own exit.
    pub fn closing(&self) {
        self.inner.closing.store(true, Ordering::SeqCst);
    }

    /// Puts this session's workspace on another endpoint, in another directory.
    ///
    /// # Invariant
    ///
    /// The caller holds the gate: a turn reads the workspace at its start and holds
    /// it for the turn, and the swap is one assignment under the lock every tool call
    /// reads it through. A move ends no channel -- a channel belongs to the machine
    /// it was started on, and the move only changes where the next one starts.
    pub async fn relocate(
        &self,
        local: Endpoint,
        remote: Option<crate::remotes::RemoteConfig>,
        cwd: Option<PathBuf>,
    ) -> anyhow::Result<()> {
        let fresh = Workspace::of(local, remote, cwd)?;
        {
            let mut workspace = self
                .inner
                .workspace
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            *workspace = fresh;
        }
        tracing::info!(session = %self.inner.name, "session moved");
        Ok(())
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

    /// What this session is for. Read at the start of every tool call.
    pub fn mode(&self) -> crate::explore::Mode {
        if self.explore().is_some() {
            crate::explore::Mode::Explore
        } else {
            crate::explore::Mode::Work
        }
    }

    /// The look a program call of this session takes when it ends, if it takes one.
    pub fn explore(&self) -> Option<Arc<crate::explore::Watch>> {
        self.inner
            .explore
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// Replaces the mode. The caller has already written it down; this is the copy
    /// tool calls read.
    ///
    /// # Invariant
    ///
    /// The look survives a mode that stays a read, so a change of some other setting
    /// does not throw away what the next call is compared against.
    pub fn set_mode(&self, mode: crate::explore::Mode) {
        let mut held = self.inner.explore.lock().unwrap_or_else(|e| e.into_inner());
        *held = match mode {
            crate::explore::Mode::Explore => held.take().or_else(|| crate::explore::watch_of(mode)),
            crate::explore::Mode::Work => None,
        };
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
        // A new turn is not a waiting one: a shutdown that kept the last turn for
        // the next start has no claim on this one.
        self.inner.resumable.store(false, Ordering::SeqCst);
        self.inner.interrupted.store(false, Ordering::SeqCst);
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

    /// What a tool call reads to know a `cancel` ended the turn it is inside.
    pub fn stop_signal(&self) -> StopSignal {
        StopSignal {
            resumable: self.inner.resumable.clone(),
            interrupted: self.inner.interrupted.clone(),
            waiting: self.inner.waiting.clone(),
            until: self.inner.until.clone(),
        }
    }

    /// Keeps this turn's row for the next start: a `cancel` found it waiting.
    pub fn mark_resumable(&self) {
        self.inner.resumable.store(true, Ordering::SeqCst);
    }

    /// Says an `interrupt_session` ended this turn, not a `cancel`.
    pub fn mark_interrupted(&self) {
        self.inner.interrupted.store(true, Ordering::SeqCst);
    }

    /// Whether the turn in flight was ended by `interrupt_session`.
    pub fn interrupted(&self) -> bool {
        self.inner.interrupted.load(Ordering::SeqCst)
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
        self.workspace()
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
        self.workspace()
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .owner()
    }

    /// The peer this session's directory is on, when it is not this machine.
    ///
    /// # Invariant
    ///
    /// Read from the live workspace, not from the row: the row is what a restart hands
    /// back, and a move swaps the workspace under the lock every tool reads them through.
    pub fn remote_name(&self) -> Option<String> {
        self.workspace()
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remote_name()
            .map(str::to_string)
    }

    /// Whether this session's workspace run somewhere other than this machine.
    pub fn on_remote(&self) -> bool {
        self.workspace()
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .has_remote()
    }

    /// Keeps the row's directory equal to where an endpoint put this session's shell.
    ///
    /// # Invariant
    ///
    /// Only a session whose workspace are not this machine's writes it, and only once a
    /// shell has connected: the row is what the next start hands back to the endpoint.
    /// A local row stays the directory the session was made in. See
    /// `docs/notes/server-remote.md`.
    pub fn adopt_reported_cwd(&self) {
        // Only the remote's report is a fact about the row.
        let cwd = {
            let workspace = self
                .inner
                .workspace
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            if workspace.owner() != Machine::Remote {
                return;
            }
            workspace.dir(Machine::Remote)
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
#[derive(Clone)]
pub struct InboxSignal {
    epoch: Arc<AtomicU64>,
    since: u64,
}

impl InboxSignal {
    /// True if a message arrived after this signal was taken.
    pub fn arrived(&self) -> bool {
        self.epoch.load(Ordering::SeqCst) != self.since
    }

    /// A signal nothing can raise.
    ///
    /// # Invariant
    ///
    /// A job that outlives the turn which asked for it is not owed to anybody's
    /// message: its own count, so a person speaking ends the turn and not the work.
    pub fn never() -> Self {
        Self {
            epoch: Arc::new(AtomicU64::new(0)),
            since: 0,
        }
    }
}

/// A `cancel` reaching a turn that is inside a tool call.
///
/// # Invariant
///
/// The one place a tool learns what the stop means for its turn: a `wait` marks
/// itself here, and a cancel that finds the mark keeps the turn for the next start.
/// Every other call is interrupted and owes nothing. See
/// `docs/notes/server-commands.md`.
#[derive(Clone)]
pub struct StopSignal {
    resumable: Arc<AtomicBool>,
    /// Set by `interrupt_session`: the turn ends here, and no restart continues it.
    interrupted: Arc<AtomicBool>,
    waiting: Arc<AtomicBool>,
    /// When this wait is due to end, epoch ms; 0 is no wait. The wall clock, not a timer.
    until: Arc<AtomicI64>,
}

impl StopSignal {
    /// A signal nothing asked to stop: what a program of its own runs under.
    ///
    /// # Invariant
    ///
    /// Nothing owes this call to a turn, so nothing can ask it to stop and nothing
    /// has to be replayed. See `docs/notes/server-tools-worker.md`.
    pub fn idle() -> Self {
        Self {
            resumable: Arc::new(AtomicBool::new(false)),
            interrupted: Arc::new(AtomicBool::new(false)),
            waiting: Arc::new(AtomicBool::new(false)),
            until: Arc::new(AtomicI64::new(0)),
        }
    }

    /// Says this call is waiting, with the deadline a `cancel` has to hand on.
    ///
    /// # Invariant
    ///
    /// The mark is cleared by the guard's drop, which an aborted call also runs: a
    /// wait that ended for any reason is not a wait a later cancel can find, and
    /// its deadline is not one a restart should resume.
    pub fn begin_wait(&self, wait: std::time::Duration) -> WaitGuard {
        let until =
            crate::store::now_millis() + i64::try_from(wait.as_millis()).unwrap_or(i64::MAX);
        self.until.store(until, Ordering::SeqCst);
        self.waiting.store(true, Ordering::SeqCst);
        WaitGuard {
            waiting: self.waiting.clone(),
            until: self.until.clone(),
        }
    }

    /// Whether a call is inside `wait` right now.
    pub fn is_waiting(&self) -> bool {
        self.waiting.load(Ordering::SeqCst)
    }

    /// The instant this session's wait ends, when it is inside one.
    ///
    /// # Invariant
    ///
    /// Read by a stop, written into `sessions.waiting_until`: SPEC keeps the wait
    /// rather than the turn, so what has to survive the process is when it was due
    /// to end, never how long it had been running.
    pub fn wait_until(&self) -> Option<i64> {
        match self.is_waiting() {
            true => Some(self.until.load(Ordering::SeqCst)),
            false => None,
        }
    }

    /// Whether a `cancel` decided this turn is the next start's to resume.
    pub fn resumable(&self) -> bool {
        self.resumable.load(Ordering::SeqCst)
    }

    /// Whether `interrupt_session` decided this turn, rather than a `cancel`.
    ///
    /// # Invariant
    ///
    /// SPEC gives the rerun to `cancel` and to it alone: an interrupted turn stops
    /// where it stands, so the sentence a cut call leaves says which of the two it was.
    pub fn interrupted(&self) -> bool {
        self.interrupted.load(Ordering::SeqCst)
    }
}

/// Clears the waiting mark however the call leaves, including an abort.
pub struct WaitGuard {
    waiting: Arc<AtomicBool>,
    until: Arc<AtomicI64>,
}

impl Drop for WaitGuard {
    fn drop(&mut self) {
        self.until.store(0, Ordering::SeqCst);
        self.waiting.store(false, Ordering::SeqCst);
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
    /// The same facts with a native `PATH`, for programs that are not workspace.
    script_env: Vec<(String, String)>,
    /// The named peers a session may run on. See `docs/notes/server-remote.md`.
    remotes: Vec<crate::remotes::RemoteConfig>,
    live: Arc<Mutex<HashMap<String, SessionHandle>>>,
    /// One shell per remote, opened only to list a directory for completion.
    ///
    /// # Invariant
    ///
    /// Kept apart from every session's workspace and never locked while `live` is:
    /// a directory listing is answered while a turn is running, and a Tab must not
    /// queue behind one. See `docs/notes/server-remote.md`.
    /// One file-reading connection per remote, and nobody's shell.
    ///
    /// # Invariant
    ///
    /// Not a session's shell: a message is delivered while that session's command may
    /// be running, and waiting for its lock would make an instant inject wait for a
    /// build. Keyed by remote, so two sessions on one peer share one connection.
    readers: Arc<Mutex<HashMap<String, Arc<crate::remote::Reader>>>>,
}

/// What one session adds to either base environment: its name, its model, its machine.
///
/// # Invariant
///
/// Read from the rows every time, so a `set_session --model` is what the next child
/// sees: a shell, a Python and an entry asked the same question give one answer.
fn session_vars(store: &Store, name: &str) -> Vec<(String, String)> {
    let mut env = Vec::new();
    // A shell cannot learn its own session's name from the outside.
    env.push(("GONGGA_SESSION".to_string(), name.to_string()));
    // ... nor its model or machine: a child is placed from these,
    // not a flagless `set_session`; sticky fill would move it.
    if let Some(row) = store.get_session(name).ok().flatten() {
        if !row.model.trim().is_empty() {
            env.push(("GONGGA_MODEL".to_string(), row.model.clone()));
        }
        if let Some(remote) = row.remote.filter(|remote| !remote.trim().is_empty()) {
            env.push(("GONGGA_REMOTE".to_string(), remote));
        }
    }
    env
}

impl SessionManager {
    pub fn new(
        store: Store,
        bash: BashConfig,
        model: String,
        detached: DetachedRegistry,
        shell_env: Vec<(String, String)>,
        script_env: Vec<(String, String)>,
        remotes: Vec<crate::remotes::RemoteConfig>,
    ) -> Self {
        Self {
            store,
            bash,
            model,
            detached,
            shell_env,
            script_env,
            remotes,
            live: Arc::new(Mutex::new(HashMap::new())),
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
        // A peer's directory is one `read_dir` through the connection kept to it.
        let reader = self.reader_for(remote).ok_or_else(|| {
            anyhow::anyhow!("remote {remote:?} keeps no file connection for a listing")
        })?;
        let mut entries: Vec<(String, bool)> = reader
            .list(directory)
            .await?
            .iter()
            .filter(|entry| entry.is_dir || entry.is_file)
            .map(|entry| (entry.name.clone(), entry.is_dir))
            .collect();
        entries.sort();
        Ok(entries)
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

    /// The remote names a session may name, in the config's order.
    pub fn remote_names(&self) -> Vec<String> {
        self.remotes.iter().map(|r| r.name.clone()).collect()
    }

    /// The detached jobs of every session.
    pub fn detached(&self) -> DetachedRegistry {
        self.detached.clone()
    }

    /// Marks every live session's way out, so a call in flight can say what ended it.
    pub fn closing(&self) {
        let handles: Vec<SessionHandle> = self.live().values().cloned().collect();
        for handle in handles {
            handle.closing();
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
    /// Never moves an existing session: a move ends workspace, and only
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
                // Just created: no turn of it has ended yet.
                ended_at: None,
                messages: 0,
                // A session that just came into existence has measured nothing.
                usage: protocol::Usage::default(),
                remote: recorded.clone(),
                cache_tier: None,
                cache_keepalive: false,
                // Born working; `set_session` moves it in the same command.
                mode: crate::explore::Mode::Work,
            },
            &mut live,
        )
    }

    /// The handle if the session is already in memory, without spawning anything.
    pub fn peek(&self, name: &str) -> Option<SessionHandle> {
        self.live().get(name).cloned()
    }

    /// Looks a session up, spawning its workspace on first use.
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
    /// answered with and where the workspace run are one fact. A name for this machine
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

    fn local_endpoint(&self) -> anyhow::Result<Endpoint> {
        let program = self
            .bash
            .program
            .clone()
            .ok_or_else(|| anyhow::anyhow!("no bash available; set [tools.bash] program"))?;
        Ok(Endpoint::local(program))
    }

    /// Puts an existing session's workspace on `remote`, in `cwd`.
    ///
    /// # Invariant
    ///
    /// Never applied under a running turn, always remembered instead. The turn holds
    /// the gate, read the shell set at its start and keeps it, so a move landing
    /// under it would end workspace a tool call is using. Queueing rather than refusing
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
        // Resolved before anything is queued: an unknown name must not tear down workspace.
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

    /// Ends the workspace and records the new placement. The caller holds the gate.
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
        session
            .relocate(self.local_endpoint()?, remote, wanted.clone())
            .await?;
        // The reading side moves with the workspace.
        session.set_reader(self.reader_for(request.remote.as_deref()));
        // A move states the placement; a shell's report does not.
        session.set_placed(wanted.clone());
        // The row is what the next start hands back to the endpoint, so it moves
        // with the workspace rather than after them.
        let placed = match (&wanted, session.cwd().to_string_lossy().to_string()) {
            (Some(path), _) => path.to_string_lossy().into_owned(),
            (None, reported) if !reported.is_empty() => reported,
            (None, _) => String::new(),
        };
        self.store
            .set_session_placement(session.name(), &placed, request.remote.as_deref())?;
        Ok(MoveReport::default())
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
                 ends the workspace on the machine it is leaving, so it is `set_session \
                 --remote` that does that, not creating the session."
            );
        }
        Ok(())
    }

    /// What a session's workspace run on, as its row records it.
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
        let workspace = Workspace::of(self.local_endpoint()?, remote, cwd.clone())?;
        let reader = self.reader_for(row.remote.as_deref());
        tracing::debug!(session = %row.name, model = %row.model, cwd = ?cwd, "session workspace ready");
        let handle = SessionHandle {
            inner: Arc::new(SessionInner {
                id: row.id,
                name: row.name.clone(),
                store: self.store.clone(),
                workspace: Arc::new(Mutex::new(workspace)),
                script_env: self.script_env.clone(),
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
                    cache_tier: row.cache_tier,
                    cache_keepalive: row.cache_keepalive,
                }),
                explore: Mutex::new(crate::explore::watch_of(row.mode)),
                deleted: AtomicBool::new(false),
                resumable: Arc::new(AtomicBool::new(false)),
                interrupted: Arc::new(AtomicBool::new(false)),
                waiting: Arc::new(AtomicBool::new(false)),
                until: Arc::new(AtomicI64::new(0)),
                closing: Arc::new(AtomicBool::new(false)),
                reader: Mutex::new(reader),
                placed: Mutex::new((!row_cwd.trim().is_empty()).then(|| PathBuf::from(&row_cwd))),
                open: Mutex::new(Vec::new()),
                pending_move: Mutex::new(None),
            }),
        };
        // A session that read before the restart reads still: this is its baseline.
        crate::explore::prime(handle.explore(), handle.cwd());
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

    /// Every session with a live handle, i.e. one whose workspace exist in memory.
    pub fn live_sessions(&self) -> Vec<SessionHandle> {
        self.live().values().cloned().collect()
    }
}

/// One word for a POSIX shell: single-quoted, with an embedded quote escaped.
fn quote_word(word: &str) -> String {
    format!("'{}'", word.replace('\'', "'\\''"))
}

/// The one spelling of a session's directory, as the prompt and the layouts key it.
///
/// # Invariant
///
/// Two spellings of one directory render one string: a re-spelling would rewrite the
/// cached tail, and two sessions of that directory would plan from two layouts. See
/// `docs/notes/server-prefix-cache.md`.
pub fn stated(cwd: &std::path::Path) -> String {
    let resolved = std::fs::canonicalize(cwd).unwrap_or_else(|_| cwd.to_path_buf());
    let text = resolved.display().to_string();
    match text.strip_prefix(r"\\?\") {
        Some(rest) => rest.to_string(),
        None => text,
    }
}
