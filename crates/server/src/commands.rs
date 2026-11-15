//! The command surface: catalog and dispatch.
//! See `docs/notes/server-commands.md`.

use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use protocol::{
    Args, CommandSpec, CommandTarget, CompletionSource, Event, LogLevel, REPLAY_TURN,
    ResponseEvent, ResponsePart, ToolAnswerEvent, ToolStream, TurnOutcome,
};
use serde_json::json;

use tokio_util::sync::CancellationToken;

use crate::agent::Agent;
use crate::bus::EventBus;
use crate::session::{SessionHandle, SessionManager, Tuning};
use crate::store::{Dream, MessageRow, PendingTask, Store, Transcript};

/// How long `cancel` waits before pulling the plug, so the acknowledgement
/// still reaches whoever asked for it.
const SHUTDOWN_NOTICE: Duration = Duration::from_millis(250);

/// How long one command may run before the harness calls it a bug.
///
/// # Invariant
///
/// SPEC `## Server Cmd`: running out of this clock is a harness bug, never a refusal of
/// the call. The number is `CALL_TIMEOUT_MS`, which no caller reads or is told. It exists
/// because a command that waits on a mechanism used to hold its caller for as long as the
/// mechanism did.
const COMMAND_TIMEOUT: Duration = Duration::from_millis(crate::config::CALL_TIMEOUT_MS);

/// The commands whose own argument is the clock, and that are therefore not cut here.
///
/// # Invariant
///
/// `input_channel` waits for as long as its `wait` says -- that is the call's own clock,
/// and cutting it would break a read that is working. `run_skill` is started under the
/// skill runner's own bound. `cancel` is the stop itself: cutting it would leave the
/// acknowledgement it owes unwritten.
const OWN_CLOCK: &[&str] = &["input_channel", "run_skill", "cancel"];

/// The heartbeat's whole content: a clock reading, so a warm prefix is all it buys.
fn time_notice() -> String {
    format!("(time notice: {})", crate::clock::now())
}

/// A dream's whole instruction: documents only, and the turn ends when they are
/// in order.
const DREAM_NOTICE: &str = "(dream notice: you are a dream, a disposable copy of the session that spawned you. \
Your only work is to organize the workspace's documents: consolidate, deduplicate and retire stale notes \
under .agents/notes/ and docs/notes/. Write no code, write no tests, and make no git writes. \
The files you leave are the memory; this session is discarded when your turn ends. \
End the turn when the documents are in order.)";

#[derive(Clone)]
pub struct Dispatch {
    pub agent: Arc<Agent>,
    pub sessions: SessionManager,
    pub store: Store,
    pub bus: EventBus,
    /// What sessions asked to be told about, and the matcher that tells them.
    pub subscriptions: Arc<crate::subscriptions::Subscriptions>,
    /// Commands registered by scripts, and the way to run one.
    pub scripts: Arc<crate::script::Scripts>,
    /// Commands the content declares: a skill's function, published under its own name.
    pub declared: Arc<crate::content_commands::ContentCommands>,
    /// The machine's LLM roster: what `set_session --model` is checked against.
    pub providers: crate::providers::Handle,
    /// Set as soon as cancellation starts, before the delayed socket shutdown.
    pub shutting_down: Arc<AtomicBool>,
    /// Pulled after the cancel acknowledgement has had a chance to leave.
    pub shutdown: CancellationToken,
    /// The session rows this server last published, so a change is sent as the
    /// one row that changed.
    ///
    /// # Invariant
    ///
    /// The picture the bus last handed out, never a cache of the database: the
    /// difference against it is what a client is missing, and comparing with a
    /// stored table it never saw would announce rows nobody needs. See
    /// [`publish_sessions`].
    pub published: std::sync::Arc<std::sync::Mutex<Vec<protocol::SessionInfo>>>,
}

/// Who asked for a command, as far as the server can tell.
///
/// # Invariant
///
/// One place answers "who": a log row about an act of the server's own takes its
/// words from here, so the same act asked two ways is told apart in the record.
/// A client names itself (`ClientInfo::label`) and says the session its shell was
/// in; a stop that never crossed the wire has no client to name, and says which
/// ask reached the process. See `docs/notes/server-commands.md`.
#[derive(Debug, Clone)]
pub enum Origin {
    /// A client asked: its label, its peer, the session it ran from and the program
    /// that started it, where those are known.
    Client {
        label: String,
        peer: SocketAddr,
        session: Option<String>,
        /// What started the client, as `name/pid`: the one fact a `cancel` row needs
        /// and the client's own pid cannot give.
        parent: Option<String>,
    },
    /// No client asked: a signal reached this process, or the process it follows ended.
    Signal { reason: String },
    /// A tool ran it, through the command layer.
    ///
    /// # Invariant
    ///
    /// What a person may do, a tool may: the `self-control` skill is that promise.
    Tool { session: String },
}

impl Origin {
    /// The session the command was run from, when a shell ran it.
    pub fn session(&self) -> Option<&str> {
        match self {
            Origin::Client { session, .. } => session.as_deref(),
            Origin::Tool { session } => Some(session.as_str()),
            Origin::Signal { .. } => None,
        }
    }

    /// Who asked, in the words a log row keeps for it.
    pub fn describe(&self) -> String {
        match self {
            Origin::Client {
                label,
                peer,
                session,
                parent,
            } => {
                let started = match parent.as_deref().map(str::trim) {
                    Some(parent) if !parent.is_empty() => format!(" (started by {parent})"),
                    _ => String::new(),
                };
                match session.as_deref().map(str::trim) {
                    Some(session) if !session.is_empty() => {
                        format!("{label} at {peer}, from session {session}{started}")
                    }
                    _ => format!("{label} at {peer}{started}"),
                }
            }
            Origin::Signal { reason } => reason.clone(),
            Origin::Tool { session } => format!("a tool in session {session}"),
        }
    }
}

/// A durable turn plus the session slot reserved before any task is spawned.
struct TurnWork {
    session: crate::session::SessionHandle,
    tasks: Vec<i64>,
    epoch: u64,
    gate: tokio::sync::OwnedMutexGuard<()>,
}

/// Hands one durable turn to the agent.
///
/// # Invariant
///
/// A turn's survival is its pending row in the database, not an in-memory count:
/// `cancel` interrupts the turn and the process leaves without waiting for it, and
/// the next start resumes whatever the row still owes. See
/// `docs/notes/server-commands.md`.
fn spawn_turn(d: &Dispatch, work: TurnWork) {
    if d.shutting_down.load(Ordering::SeqCst) {
        // The turn's pending row stays in the database, so the next start owes it.
        return;
    }
    let agent = d.agent.clone();
    let sessions = d.sessions.clone();
    let bus = d.bus.clone();
    // Read before the turn starts: its first step consumes the mark.
    let keepalive_d = Arc::new(d.clone());
    let keepalive_session = work.session.clone();
    let keepalive_model = d.agent.config.now().model.model();
    let keepalive_tuning = work.session.tuning();
    tokio::spawn(async move {
        let session = work.session.clone();
        let (outcome, interrupted) = agent
            .run_turn(&work.session, work.tasks, work.epoch, work.gate)
            .await;
        // SPEC's card: what the worktree holds now that the turn is quiet, then the
        // row that moved.
        read_worktree_state(&keepalive_d, &session).await;
        publish_sessions(&keepalive_d);
        // What the turn queued behind itself is this turn's to finish, and the
        // gate is free here without being taken.
        if session.has_move() {
            if let Some(gate) = session.try_claim() {
                match sessions.apply_queued_move(&session).await {
                    Some(Ok(report)) => bus.publish(Event::Log {
                        level: LogLevel::Info,
                        message: format!(
                            "{}: moved at the end of its turn, {} shell(s) ended",
                            session.name(),
                            report.ended
                        ),
                        session: None,
                    }),
                    Some(Err(e)) => bus.publish(Event::Log {
                        level: LogLevel::Error,
                        message: format!("{}: the queued move failed: {e}", session.name()),
                        session: None,
                    }),
                    None => {}
                }
                drop(gate);
            }
        }
        // SPEC's Dreaming: a dream is disposable, and its true end discards it.
        if keepalive_session.dream_of().is_some()
            && (outcome != TurnOutcome::Cancelled || interrupted)
        {
            discard_dream(&keepalive_d, &keepalive_session).await;
            return;
        }
        // Each turn schedules the next notice, so the chain has no last one.
        arm_keepalive(
            keepalive_d,
            keepalive_session,
            keepalive_tuning,
            keepalive_model,
        );
    });
}

/// Arms the next heartbeat: a sleeper that sends the time notice, or dreams in
/// its place.
fn arm_keepalive(d: Arc<Dispatch>, session: SessionHandle, tuning: Tuning, model: String) {
    if !tuning.cache_keepalive {
        return;
    }
    let wait_ms = d
        .agent
        .cache_fire_after_ms(&model, tuning.cache_tier.as_deref());
    if wait_ms == 0 {
        return;
    }
    let ended_at = crate::store::now_millis();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(wait_ms)).await;
        // Any request during the wait already refreshed the prefix.
        let refreshed = d
            .store
            .last_usage_at(session.id())
            .ok()
            .flatten()
            .is_some_and(|at| at > ended_at);
        if refreshed || session.busy() {
            return;
        }
        // SPEC's Dreaming: the first fire of a generation dreams in the notice's place.
        if !d.shutting_down.load(Ordering::SeqCst) && spawn_dream(&d, &session).await {
            // The source stayed idle, so no turn re-arms the chain: this fire does.
            let tuning = session.tuning();
            let model = d.agent.config.now().model.model();
            arm_keepalive(d, session, tuning, model);
            return;
        }
        let _ = deliver(d.as_ref(), session.name(), time_notice()).await;
    });
}

/// Clones the source for a generation not yet dreamed, and hands the clone its
/// notice. Returns whether this fire dreamed.
async fn spawn_dream(d: &Dispatch, source: &SessionHandle) -> bool {
    let Ok(Some(row)) = d.store.get_session(source.name()) else {
        return false;
    };
    let generation = d
        .store
        .latest_checkpoint_id(source.id())
        .ok()
        .flatten()
        .unwrap_or(0);
    if row.dreamed_checkpoint_id == Some(generation) {
        return false;
    }
    // At most one live dream per session: a running one finishes its generation first.
    if !d
        .store
        .dreams_of(source.id())
        .is_ok_and(|dreams| dreams.is_empty())
    {
        return false;
    }
    let name = format!("{}/dream/{}", source.name(), generation);
    let dream = Dream {
        source: source.id(),
        generation,
    };
    let report = match d.store.clone_session(source.name(), &name, Some(dream)) {
        Ok(report) => report,
        Err(e) => {
            tracing::warn!(session = %source.name(), error = %e, "the dream was not cloned");
            return false;
        }
    };
    // Marked before the notice: a fire that races this one must not dream twice.
    if let Err(e) = d.store.set_dreamed_checkpoint(source.id(), generation) {
        tracing::warn!(session = %source.name(), error = %e, "could not mark the dreamed generation");
    }
    d.agent.log_event(
        Some(source.id()),
        "dream_spawn",
        &json!({"dream": name, "generation": generation, "rows": report.rows}),
    );
    publish_sessions(d);
    let said = deliver(d, &name, DREAM_NOTICE.to_string()).await;
    if !said.ok {
        // A dream with no notice is row-less work: the next start's sweep discards it.
        tracing::warn!(session = %name, answer = %said.message, "the dream notice was not delivered");
    }
    tracing::info!(session = %name, source = %source.name(), generation, rows = report.rows, "dream spawned");
    true
}

/// Discards a dream whose turn truly ended, through the whole delete machinery.
async fn discard_dream(d: &Dispatch, session: &SessionHandle) {
    if session.is_deleted() {
        return;
    }
    let name = session.name().to_string();
    // On the source: the dream's own events are deleted with its row.
    d.agent
        .log_event(session.dream_of(), "dream_discard", &json!({"dream": name}));
    // A dream owns no worktree: the clone never copies one.
    let (was_busy, dropped) = prepare_delete(d, &name);
    let said = finish_delete(d, &name, was_busy, dropped, None).await;
    if !said.ok {
        tracing::warn!(session = %name, answer = %said.message, "the dream was not discarded");
    }
}

/// Claims one idle session; an inject-only recovery gets a durable turn row before
/// its inbox is delivered.
fn claim_recovery(
    d: &Dispatch,
    session: crate::session::SessionHandle,
    mut tasks: Vec<i64>,
    owed: bool,
) -> Option<TurnWork> {
    if !owed {
        // Nothing a person is waiting for: the row waits for the next real turn.
        return None;
    }
    let epoch = session.cancel_epoch();
    let gate = session.try_claim()?;
    if session.is_deleted() {
        return None;
    }
    // SPEC: restart consumes the flag and continues -- this claim is that restart.
    if let Err(e) = d.store.record_stop(session.id(), false, None) {
        tracing::warn!(
            session = %session.name(), error = %e,
            "could not clear the flags this start continued"
        );
    }
    if tasks.is_empty() {
        match d
            .store
            .add_pending_task(session.name(), "turn", &json!({"session": session.name()}))
        {
            Ok(id) => tasks.push(id),
            Err(e) => {
                tracing::warn!(session = %session.name(), error = %e, "could not record recovered turn");
                return None;
            }
        }
    }
    Some(TurnWork {
        session,
        tasks,
        epoch,
        gate,
    })
}

/// The result of one command, sent back as a `cmd_result` event.
///
/// # Invariant
///
/// `events` go to the requesting connection before the result, and nowhere else:
/// they are the command's answer in the vocabulary every client already renders,
/// not a broadcast about it.
#[derive(Debug)]
pub struct Outcome {
    pub ok: bool,
    pub message: String,
    pub data: Option<serde_json::Value>,
    /// The events this answer is made of, each with the number it was stored at.
    ///
    /// # Invariant
    ///
    /// A row that came off the store goes out with its own `seq`, because a client
    /// that keeps the server's state must key the row by that number: a row sent
    /// without one can only be appended where it arrived, never placed. Only a row
    /// that is not stored yet -- the stream still being written -- goes out with
    /// `None`. See `SPEC.md`, "Client".
    pub events: Vec<(Option<u64>, Event)>,
}

impl Outcome {
    pub fn ok(message: impl Into<String>) -> Self {
        Self {
            ok: true,
            message: message.into(),
            data: None,
            events: Vec::new(),
        }
    }

    pub fn ok_with(message: impl Into<String>, data: serde_json::Value) -> Self {
        Self {
            ok: true,
            message: message.into(),
            data: Some(data),
            events: Vec::new(),
        }
    }

    pub fn err(message: impl Into<String>) -> Self {
        Self {
            ok: false,
            message: message.into(),
            data: None,
            events: Vec::new(),
        }
    }

    /// Attaches the rows this command answers with, oldest first.
    pub fn with_events(mut self, events: Vec<(Option<u64>, Event)>) -> Self {
        self.events = events;
        self
    }
}

/// The commands the server runs itself.
///
/// # Invariant
///
/// Not the whole catalog: `Dispatch::catalog` adds the client's commands and
/// whatever scripts registered. One list, built once per greeting.
pub fn server_catalog(remotes: &[String]) -> Vec<CommandSpec> {
    vec![
        CommandSpec {
            target: CommandTarget::Server,
            name: "send_message".into(),
            description: "Send a message to a session and run a turn.".into(),
            args: vec![
                arg(
                    "message",
                    "The message text.",
                    true,
                    Some("text"),
                    CompletionSource::None,
                ),
                // SPEC: `--message <message> --session <session>`; the payload leads.
                kind_arg(
                    "session",
                    "Target session. Required: there is no implicit default.",
                    true,
                    "session",
                    Some("name"),
                    CompletionSource::Sessions,
                ),
            ],
        },
        CommandSpec {
            target: CommandTarget::Server,
            name: "new_session".into(),
            description: "Create a session under a name nobody holds, and refuse the call \
                      when one already does: the verb a caller uses to start its own."
                .into(),
            args: session_config_args(remotes, false)
                .into_iter()
                .filter(|argument| argument.name != "must_create")
                .collect(),
        },
        CommandSpec {
            target: CommandTarget::Server,
            name: "set_session".into(),
            description:
                "Create the session if it does not exist, and apply the given settings to \
                      it. Settings omitted now are left as they are."
                    .into(),
            args: session_config_args(remotes, true),
        },
        CommandSpec {
            target: CommandTarget::Server,
            name: "list_paths".into(),
            description: "List one directory on a remote, for a front-end completing `--cwd`. \
                      Not a tool the model needs: the paths of a session's own shell are 
                      read by running `ls` in it."
                .into(),
            args: vec![
                arg(
                    "remote",
                    "The remote whose filesystem to read. Omitted is this machine.",
                    false,
                    Some("name"),
                    CompletionSource::Static(remotes.to_vec()),
                ),
                arg(
                    "path",
                    "The directory to list, as the peer spells a path. Empty is where \
                     the target's shell starts.",
                    false,
                    Some("path"),
                    // What this command answers is exactly what it would list.
                    CompletionSource::None,
                ),
            ],
        },
        CommandSpec {
            target: CommandTarget::Server,
            name: "del_session".into(),
            description: "Delete a session and everything under that path: its shells, its \
                      channels and its transcript. A path that names a directory deletes 
                      every session below it."
                .into(),
            args: vec![kind_arg(
                "path",
                "Session to delete, or the directory whose sessions all go.",
                true,
                "session",
                Some("name"),
                CompletionSource::Sessions,
            )],
        },
        CommandSpec {
            target: CommandTarget::Server,
            name: "ls_session".into(),
            description: "List sessions by the prefix of their name: what each runs, \
                      where it stands, and how full its context is."
                .into(),
            args: vec![pos(
                "path",
                "The sessions whose name starts this way. Omitted is every one.",
                false,
                Some("path"),
                CompletionSource::None,
            )],
        },
        CommandSpec {
            target: CommandTarget::Server,
            name: "interrupt_session".into(),
            description: "Stop the turn one session is running: a tool that is waiting is \
                      ended and its result handed back, the stream stops, and everything 
                      received is kept. Nothing is owed to a restart -- `cancel` is the verb 
                      that continues a turn, not this one. Background jobs keep running."
                .into(),
            args: vec![kind_pos(
                "id",
                "Session whose turn should stop.",
                true,
                "session",
                Some("name"),
                CompletionSource::Sessions,
            )],
        },
        CommandSpec {
            target: CommandTarget::Server,
            name: "cancel".into(),
            description: "Interrupt every running turn and shut the server down at once. SPEC: \
                      every running session that is not waiting is flagged, and the next start 
                      continues it. This is the only stop that lets background jobs go: they are 
                      processes of their own, outlive this server and are adopted on the next 
                      start. Any other stop -- a signal, a killed process -- ends every job it 
                      started with it. 
                      Channels are ended too unless `keep_alive` asks for them: their programs 
                      are processes of their own, and one left running is adopted by the next 
                      start instead of the session losing the shell it was using."
                .into(),
            args: vec![arg(
                "keep_alive",
                "`true` leaves every channel's program running so the next start adopts it; \
                 a channel that is not kept ends with this server.",
                false,
                Some("true|false"),
                CompletionSource::None,
            )],
        },
        CommandSpec {
            target: CommandTarget::Server,
            name: "events".into(),
            description: "Read the server's event log: one row per thing that happened, oldest \
                      first, as `{id, kind, payload}`. A cursor, not a transcript -- keep the 
                      last id and ask again from there."
                .into(),
            args: vec![
                arg(
                    "since",
                    "Return rows after this log id, exclusive. Omit for the whole log; pass the \
                     `last_id` of the previous answer to continue where it stopped.",
                    false,
                    Some("id"),
                    CompletionSource::None,
                ),
                arg(
                    "kind",
                    "Only this kind of row, e.g. `turn_end`, `tool_result`, `step_timing`.",
                    false,
                    Some("kind"),
                    CompletionSource::None,
                ),
                arg(
                    "session",
                    "Only rows about this session. Rows that belong to no session (a start, a \
                     migration) are left out when this is given.",
                    false,
                    Some("name"),
                    CompletionSource::Sessions,
                ),
                arg(
                    "limit",
                    "At most this many rows, oldest first. The default is 200.",
                    false,
                    Some("n"),
                    CompletionSource::None,
                ),
            ],
        },
        CommandSpec {
            target: CommandTarget::Server,
            name: "subscribe".into(),
            description: "Name a subscription over the event stream and ask for it to be \
                      armed. It answers at once -- it does not wait for a match. A match 
                      runs `cmd` in `session`: one command, the same door the 
                      `gongga_cli` tool uses."
                .into(),
            args: vec![
                pos(
                    "pattern",
                    "A regex, matched against an event's name -- a log row's `kind` \
                     (`turn_end`, `tool_result`, `context_notice`) or another event's own 
                     (`status`, `usage`, `sessions`, `log`, `cmd_result`) -- and against 
                     `name:session` when the event is about a session, so `turn_end:child` 
                     is one session's turn ending. Unanchored, so write `^turn_end$` for 
                     exactly one name.",
                    true,
                    Some("pattern"),
                    CompletionSource::None,
                ),
                pos(
                    "cmd",
                    "What a match runs: a command's name, or the whole call as an object, \
                     `{\"name\": \"send_message\", \"args\": {\"message\": \"hi\"}}`.",
                    true,
                    Some("cmd"),
                    CompletionSource::None,
                ),
                pos(
                    "session",
                    "The session a match runs in. A tool call leaves it to the caller, \
                     which is the session it is arm ing from.",
                    false,
                    Some("name"),
                    CompletionSource::Sessions,
                ),
                arg(
                    "id",
                    "The name it is armed and dropped by. Arming a name again replaces \
                     what it held. Omit it to be given one, which `ls_subscription` 
                     reports.",
                    false,
                    Some("name"),
                    CompletionSource::None,
                ),
                arg(
                    "persistant",
                    "`true` keeps the subscription armed after it fires, so every later \
                     match is news too. Without it one match is the whole subscription.",
                    false,
                    Some("true|false"),
                    CompletionSource::Static(
                        protocol::SHOW_VALUES
                            .iter()
                            .map(|v| v.to_string())
                            .collect(),
                    ),
                ),
            ],
        },
        CommandSpec {
            target: CommandTarget::Server,
            name: "ls_subscription".into(),
            description: "List a session's armed subscriptions: the name each is dropped \
                      by, its pattern, and the command a match runs."
                .into(),
            args: vec![
                pos(
                    "path",
                    "The subscriptions whose name starts this way. Omitted is every one \
                     the session has.",
                    false,
                    Some("path"),
                    CompletionSource::None,
                ),
                kind_arg(
                    "session",
                    "Session whose subscriptions to list. Omitted is the caller's own session.",
                    false,
                    "session",
                    Some("name"),
                    CompletionSource::Sessions,
                ),
            ],
        },
        CommandSpec {
            target: CommandTarget::Server,
            name: "del_subscription".into(),
            description: "Drop one named subscription: the caller's own, unless the call \
                      says whose."
                .into(),
            args: vec![
                pos(
                    "path",
                    "The name it was armed by, or the path whose subscriptions all go.",
                    true,
                    Some("name"),
                    CompletionSource::None,
                ),
                kind_arg(
                    "session",
                    "Session that armed it. The caller's own when the call says nothing.",
                    false,
                    "session",
                    Some("name"),
                    CompletionSource::Sessions,
                ),
            ],
        },
        CommandSpec {
            target: CommandTarget::Server,
            name: "trigger_event".into(),
            description: "Publish one event under a chosen name, so the subscriptions whose \
                      pattern matches it fire. The row lands in the event log like any 
                      other, which is how a matcher sees it."
                .into(),
            args: vec![pos(
                "event",
                "The name the event is published under; a subscription's pattern is \
                 matched against it.",
                true,
                Some("name"),
                CompletionSource::None,
            )],
        },
        CommandSpec {
            target: CommandTarget::Server,
            name: "new_channel".into(),
            description: "Start a program under this session and give it a name: the \
                      command layer's own door to what the `exec` tool does."
                .into(),
            args: vec![
                pos(
                    "id",
                    "The name it is called by: a path in this session's namespace.",
                    true,
                    Some("path"),
                    CompletionSource::None,
                ),
                pos(
                    "name",
                    "The program, as a `[channel.<name>]` row names it.",
                    true,
                    Some("name"),
                    CompletionSource::None,
                ),
                kind_arg(
                    "session",
                    "Session that owns it. Omitted is the caller's own session.",
                    false,
                    "session",
                    Some("name"),
                    CompletionSource::Sessions,
                ),
            ],
        },
        CommandSpec {
            target: CommandTarget::Server,
            name: "input_channel".into(),
            description: "Write a line into a channel and answer with the part of what it \
                      said that nobody had read."
                .into(),
            args: vec![
                pos(
                    "id",
                    "The channel to write into.",
                    true,
                    Some("path"),
                    CompletionSource::None,
                ),
                pos(
                    "input",
                    "The words to write. A `[channel.<name>]` row decides the framing.",
                    true,
                    Some("input"),
                    CompletionSource::None,
                ),
                kind_arg(
                    "session",
                    "Session that owns it. Omitted is the caller's own session.",
                    false,
                    "session",
                    Some("name"),
                    CompletionSource::Sessions,
                ),
                arg(
                    "wait",
                    "How long to wait for the answer, in milliseconds. A limit on waiting, \
                     not on the program: what has not arrived stays unread for the next 
                     call. Defaults to 30000.",
                    false,
                    Some("ms"),
                    CompletionSource::None,
                ),
                arg(
                    "until",
                    "A marker to stop at. The window ends before it, and `matched` says \
                     whether it was there.",
                    false,
                    Some("text"),
                    CompletionSource::None,
                ),
            ],
        },
        CommandSpec {
            target: CommandTarget::Server,
            name: "ls_channel".into(),
            description: "List a session's channels: the name each one is called, what it \
                      runs, and how much of what it said nobody has read."
                .into(),
            args: vec![
                pos(
                    "path",
                    "The channels whose name starts this way. Omitted is every channel \
                     the session has.",
                    false,
                    Some("path"),
                    CompletionSource::None,
                ),
                kind_arg(
                    "session",
                    "Session whose channels to list. Omitted is the caller's own session.",
                    false,
                    "session",
                    Some("name"),
                    CompletionSource::Sessions,
                ),
            ],
        },
        CommandSpec {
            target: CommandTarget::Server,
            name: "del_channel".into(),
            description: "End a channel's program and forget the name, and do the same for \
                      every channel under that path. A channel that had already ended is 
                      only forgotten."
                .into(),
            args: vec![
                pos(
                    "id",
                    "Channel to end, or the path whose channels all go.",
                    true,
                    Some("path"),
                    CompletionSource::None,
                ),
                kind_arg(
                    "session",
                    "Session that owns it. Omitted is the caller's own session.",
                    false,
                    "session",
                    Some("name"),
                    CompletionSource::Sessions,
                ),
            ],
        },
        CommandSpec {
            target: CommandTarget::Server,
            name: "rename_channel".into(),
            description: "Give a channel another name. Nothing on disk moves: the name \
                      is this server's, not a path."
                .into(),
            args: vec![
                pos(
                    "id",
                    "The name it has now.",
                    true,
                    Some("path"),
                    CompletionSource::None,
                ),
                pos(
                    "new_id",
                    "The name it takes.",
                    true,
                    Some("path"),
                    CompletionSource::None,
                ),
                kind_arg(
                    "session",
                    "Session that owns it. Omitted is the caller's own session.",
                    false,
                    "session",
                    Some("name"),
                    CompletionSource::Sessions,
                ),
            ],
        },
        CommandSpec {
            target: CommandTarget::Server,
            name: "get_messages".into(),
            description: "Read the transcript: a range of sequence numbers, oldest first. \
                     Each row arrives as the event it was published as, before the result."
                .into(),
            args: vec![
                pos(
                    "range",
                    "Which rows, as a closed range of sequence numbers: `-1` the newest \
                     row, `-5:` the newest five, `5:10` rows 5 through 10, `:10` the first 
                     ten, `5:` from 5 on. A negative number counts from the end; both ends 
                     inclusive, a missing end means the newest, and a range past the end is 
                     just the rows that exist. Omitted is the whole transcript.",
                    false,
                    Some("begin:end"),
                    CompletionSource::None,
                ),
                kind_arg(
                    "session",
                    "Session to read. Omitted is the caller's own session.",
                    false,
                    "session",
                    Some("name"),
                    CompletionSource::Sessions,
                ),
                arg(
                    "replay",
                    "`true` sends the transcript as a window draws it, and says which \
                     point of the stream the answer is a picture of: the rows a window 
                     shows, the operator's own messages as their own lines, and the 
                     answer still being written. A window that reconnected asks this 
                     way; a person reading asks without it.",
                    false,
                    Some("true|false"),
                    CompletionSource::Static(
                        protocol::SHOW_VALUES
                            .iter()
                            .map(|v| v.to_string())
                            .collect(),
                    ),
                ),
            ],
        },
        CommandSpec {
            target: CommandTarget::Server,
            name: "clone_session".into(),
            description: "Copy a session's transcript into a name that is free, row for row, \
                      at the same `seq`. The copy reads exactly as the source does; a turn 
                      it runs later is its own."
                .into(),
            args: vec![
                kind_arg(
                    "id",
                    "The session to copy. It keeps its own rows: a clone reads the source, \
                     it does not take anything away.",
                    true,
                    "session",
                    Some("name"),
                    CompletionSource::Sessions,
                ),
                pos(
                    "new_id",
                    "The name the copy takes. It must be free: a copy that joined an \
                     existing transcript would be two conversations under one name.",
                    true,
                    Some("name"),
                    CompletionSource::None,
                ),
            ],
        },
        CommandSpec {
            target: CommandTarget::Server,
            name: "sync_session".into(),
            description: "Re-send a session's state as deltas after its rows were written                       directly: the session row, then each row in the range as the event it                       was published as, then where the transcript ends. A stamp that moved                       backwards is a cut, which a window answers by forgetting and refilling."
                .into(),
            args: vec![
                kind_arg(
                    "id",
                    "The session to re-sync. Its rows are already written; this only tells                      every window what changed.",
                    true,
                    "session",
                    Some("name"),
                    CompletionSource::Sessions,
                ),
                pos(
                    "trajectory",
                    "Which rows to re-send, as a closed range of sequence numbers: `-5:` the                      newest five, `5:10` rows 5 through 10, `5:` from 5 on. A negative number                      counts from the end; both ends inclusive. Omitted is the whole transcript.",
                    false,
                    Some("begin:end"),
                    CompletionSource::None,
                ),
            ],
        },
        CommandSpec {
            target: CommandTarget::Server,
            name: "ls_skill".into(),
            description: "List the skills a session can open, one row per skill, under \
                      `path`. A row's `id` is the skill's own name, and `bucket` says \
                      which root it came from: `project` for the checkout's own, `harness` \
                      for the home's, `general` for the machine's. A path is either a \
                      skill's name or a bucket's word, so `ls_skill harness` is every \
                      skill the home installed and no path is every skill there is."
                .into(),
            args: vec![
                pos(
                    "path",
                    "Where to look: a skill's name, or a bucket's word (`harness`, `general`, \
                     `project`), or nothing for all of them. Matched by component, so `har` \
                     matches nothing.",
                    false,
                    Some("path"),
                    CompletionSource::None,
                ),
                kind_pos(
                    "session",
                    "Whose skills: its directory is the `project` layer, and the machine \
                     it stands on decides whether that layer is read at all. Omitted is the caller's own session.",
                    false,
                    "session",
                    Some("name"),
                    CompletionSource::Sessions,
                ),
            ],
        },
        CommandSpec {
            target: CommandTarget::Server,
            name: "run_skill".into(),
            description: "Run one skill: `cmd` gives it its call, the way `subscribe` \
                      names the command a match runs. A skill that is a Python package \
                      runs the function `cmd.name` publishes, with `cmd.args` as that \
                      function's keyword arguments; an entry skill starts the file its \
                      frontmatter declares, with the call as its argv. It runs on this \
                      machine in the session's directory, and its output and exit code \
                      are the answer. A skill that is only a body is refused, naming the \
                      file to read instead."
                .into(),
            args: vec![
                pos(
                    "id",
                    "The skill to run, as `ls_skill` spells it: its name, and nothing above it.",
                    true,
                    Some("path"),
                    CompletionSource::None,
                ),
                pos(
                    "cmd",
                    "The call the skill runs with: a bare word, or the whole call as an \
                     object, `{\"name\": \"fetch\", \"args\": {\"url\": \"https://example.com\"}}`. \
                     A package's function takes `args` as keyword arguments, an object of \
                     named ones; an entry takes it as words, an object as `--key value` and \
                     a list as written.",
                    true,
                    Some("cmd"),
                    CompletionSource::None,
                ),
                kind_pos(
                    "session",
                    "The session it runs as: its directory is where it runs, and the call \
                     is that session's work. Omitted is the caller's own session.",
                    false,
                    "session",
                    Some("name"),
                    CompletionSource::Sessions,
                ),
            ],
        },
    ]
}

/// SPEC's `config: SessionConfig`, or -- for the command whose argument is a struct
/// update -- `SessionConfig{..}`.
///
/// # Invariant
///
/// One shape, two readings, and the reading is the argument's: `set_session` says
/// what changes, so a sticky entry may not fill it (it would restate the whole
/// config); `new_session` takes a value, so a field with no default is filled.
fn config_arg(remotes: &[String], update: bool) -> protocol::ArgSpec {
    let mut spec = arg(
        "config",
        "The session's settings as SPEC's SessionConfig, one JSON object: model_config (model as {provider, model}, thinking, ctx_limit like 70%, cache_tier), workspace (remote, cwd, worktree), keep_alive and live_compact. Every field is optional: a set_session delta keeps what it does not name, and a new session takes the configured defaults. remote and cache_tier take none to clear. worktree is read when the session is created and never after: it makes a branch and a worktree of that name in the repository the cwd sits in, and the session works in it.",
        false,
        Some("json"),
        CompletionSource::None,
    )
    .with_structure(crate::session_config::schema(remotes));
    // SPEC: `SessionConfig := {...} # sticky`, so the whole object is one entry.
    spec.kind = Some("config".into());
    spec.update = update;
    spec
}

/// A flag argument, spelled `--name <value>`.
fn arg(
    name: &str,
    description: &str,
    required: bool,
    value_hint: Option<&str>,
    completions: CompletionSource,
) -> protocol::ArgSpec {
    arg_spec(
        name,
        description,
        required,
        false,
        None,
        value_hint,
        completions,
    )
}

/// A positional argument, spelled `<name>` in the spec's own order.
fn pos(
    name: &str,
    description: &str,
    required: bool,
    value_hint: Option<&str>,
    completions: CompletionSource,
) -> protocol::ArgSpec {
    arg_spec(
        name,
        description,
        required,
        true,
        None,
        value_hint,
        completions,
    )
}

/// A named argument a sticky entry fills: `kind` is the entry's key.
fn kind_arg(
    name: &str,
    description: &str,
    required: bool,
    kind: &str,
    value_hint: Option<&str>,
    completions: CompletionSource,
) -> protocol::ArgSpec {
    arg_spec(
        name,
        description,
        required,
        false,
        Some(kind),
        value_hint,
        completions,
    )
}

fn kind_pos(
    name: &str,
    description: &str,
    required: bool,
    kind: &str,
    value_hint: Option<&str>,
    completions: CompletionSource,
) -> protocol::ArgSpec {
    let mut spec = pos(name, description, required, value_hint, completions);
    spec.kind = Some(kind.to_string());
    spec
}

fn arg_spec(
    name: &str,
    description: &str,
    required: bool,
    positional: bool,
    kind: Option<&str>,
    value_hint: Option<&str>,
    completions: CompletionSource,
) -> protocol::ArgSpec {
    protocol::ArgSpec {
        type_of: type_of(name).to_string(),
        name: name.into(),
        description: description.into(),
        required,
        positional,
        kind: kind.map(str::to_string),
        value_hint: value_hint.map(str::to_string),
        completions,
        accepts: None,
        sticky_to: None,
        structure: None,
        update: false,
    }
}

/// SPEC's type for one of these arguments, by the name it binds.
///
/// # Invariant
///
/// One table for the whole catalog: a name means one argument wherever it appears, the
/// same way a `kind` means one sticky entry. `protocol::argtype` maps the spelling.
/// Every boolean or count argument is named here: the fallback is a string, and a
/// missed name publishes a type the command refuses.
fn type_of(name: &str) -> &'static str {
    match name {
        "keep_alive" | "must_create" | "persistant" | "live_compact_enabled" | "replay" => "bool",
        "since"
        | "limit"
        | "wait"
        | "block_size"
        | "box_edge"
        | "time"
        | "live_compact_interval_tokens" => "int",
        "cmd" => "Cmd",
        "config" => "SessionConfig",
        "event" => "Event",
        "range" | "trajectory" => "Range",
        _ => "str",
    }
}

/// The arguments a session is created or re-tuned with: SPEC's `SessionConfig`.
///
/// # Invariant
///
/// One list, because `new_session` and `set_session` differ in one thing only -- whether
/// a name already taken is refused or joined.
fn session_config_args(remotes: &[String], update: bool) -> Vec<protocol::ArgSpec> {
    vec![
        // SPEC: `--id <session>(sticky)`. A sticky entry keys on the kind `session`.
        kind_arg(
            "id",
            "Session to create or reconfigure.",
            true,
            "session",
            Some("name"),
            CompletionSource::Sessions,
        ),
        // SPEC: `config: SessionConfig` -- the nested settings, one JSON object.
        config_arg(remotes, update),
        kind_arg(
            "mode",
            "What this session may do to the workspace. work (the default) writes as it likes; explore refuses write_file and replace outright and tells the session which paths a bash, exec, python or skill call changed when it ended. Not a jail: a program that writes is told, not stopped, so read the changed paths and put them back before answering.",
            false,
            "mode",
            Some("work|explore"),
            CompletionSource::Static(vec!["work".into(), "explore".into()]),
        ),
        // SPEC's `LiveCompact`: the session's own opt-in and threshold; unset follows `[agent]`.
        arg(
            "live_compact_enabled",
            "Whether the server asks this session to write checkpoint summaries. Unset follows the home's `[agent] live_compact_enabled`; switched on, the starting checkpoint is the transcript's end.",
            false,
            Some("true|false"),
            CompletionSource::Static(vec!["true".into(), "false".into()]),
        ),
        arg(
            "live_compact_interval_tokens",
            "Input-token growth between this session's checkpoints. Unset follows the home's `[agent] live_compact_interval_tokens`; a count of tokens is the threshold, never the switch.",
            false,
            Some("tokens"),
            CompletionSource::None,
        ),
        arg(
            "must_create",
            "true refuses the command unless the session does not exist yet, so a caller that means to start its own session cannot join another one's transcript by picking a name that was already taken.",
            false,
            Some("true|false"),
            CompletionSource::Static(
                protocol::SHOW_VALUES
                    .iter()
                    .map(|s| s.to_string())
                    .collect(),
            ),
        ),
    ]
}

/// Runs one command, on behalf of whoever `origin` names.
///
/// # Invariant
///
/// `origin` is not the `session` argument: that one is who the command acts on,
/// while the origin is who asked. `subscribe` needs the session the command was
/// run *from*, and `cancel` needs to be able to say who asked, which is the
/// difference between a window's own goodbye and somebody else's.
/// What a tool can ask the command layer to do: `gongga_cli`, and a row that names one.
///
/// # Invariant
///
/// A trait object, installed once, holding a weak reference to the dispatch: the
/// dispatch owns the agent, and the agent's tools are what call this, so a strong
/// reference would keep the two alive for the life of the process.
pub trait CmdLayer: Send + Sync {
    fn run<'a>(
        &'a self,
        session: &'a str,
        name: &'a str,
        args: Args,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Outcome> + Send + 'a>>;
}

static LAYER: std::sync::OnceLock<std::sync::Arc<dyn CmdLayer>> = std::sync::OnceLock::new();

/// Points this process at the command layer. Called once, after the dispatch exists.
pub fn install_layer(layer: std::sync::Arc<dyn CmdLayer>) -> bool {
    LAYER.set(layer).is_ok()
}

/// The command layer, when this process has one: a worker has none, and says so.
pub fn layer() -> Option<std::sync::Arc<dyn CmdLayer>> {
    LAYER.get().cloned()
}

/// Runs one command, and gives up on one that outlives the harness's own clock.
///
/// # Invariant
///
/// SPEC `## Server Cmd`: every command has an internal timeout, and the answer it leaves
/// when that timeout runs out says the fault is the harness's. The catalogue, the
/// required arguments and the handler are all [`run_now`]'s, so this is the one door and
/// no second copy of the dispatch decides anything.
pub async fn dispatch(d: &Dispatch, name: &str, args: &Args, origin: &Origin) -> Outcome {
    if OWN_CLOCK.contains(&name) {
        return run_now(d, name, args, origin).await;
    }
    match tokio::time::timeout(COMMAND_TIMEOUT, run_now(d, name, args, origin)).await {
        Ok(outcome) => outcome,
        Err(_) => {
            tracing::error!(
                command = %name,
                timeout = ?COMMAND_TIMEOUT,
                "a command ran out of its internal timeout: this is a harness bug"
            );
            Outcome::err(format!(
                "{name} did not answer within {}s and was abandoned: that is a harness \
                 bug, not a refusal of the call",
                COMMAND_TIMEOUT.as_secs()
            ))
        }
    }
}

/// One command, run as far as it goes.
async fn run_now(d: &Dispatch, name: &str, args: &Args, origin: &Origin) -> Outcome {
    // One catalog decides existence, runner and required args, for every kind.
    let spec = match resolve(&d.catalog().await, name, args) {
        Ok(spec) => spec,
        Err(refusal) => return refusal,
    };
    if spec.target == CommandTarget::ServerScript {
        return d.scripts.run(name, args).await;
    }
    // Before the tool arm: a row forwarding to this name would hand it back for ever.
    if let Some(route) = d.declared.route(name).await {
        return declared_command(d, &route, args, origin.session()).await;
    }
    match name {
        "send_message" => send_message(d, args, origin.session()).await,
        "set_session" => set_session(d, args).await,
        // SPEC `new_session` is `set_session` that refuses a name already taken.
        "new_session" => {
            let mut owned = args.clone();
            owned.insert("must_create", true);
            set_session(d, &owned).await
        }
        "del_session" => del_session(d, args).await,
        "list_paths" => list_paths(d, args).await,
        "interrupt_session" => interrupt_session(d, args).await,
        "cancel" => {
            // SPEC's flag: `cancel` takes it, the signal path passes false.
            let keep_alive = match flag_of(args, "keep_alive") {
                Ok(kept) => kept.unwrap_or(false),
                Err(e) => return Outcome::err(e),
            };
            cancel(d, origin, keep_alive).await
        }
        "get_messages" => get_messages(d, args, origin.session()).await,
        "new_channel" => new_channel(d, args, origin.session()).await,
        "input_channel" => input_channel(d, args, origin.session()).await,
        "rename_channel" => rename_channel(d, args, origin.session()).await,
        "ls_channel" => ls_channel(d, args, origin.session()).await,
        "del_channel" => del_channel(d, args, origin.session()).await,
        "ls_session" => ls_session(d, args).await,
        "clone_session" => clone_session(d, args).await,
        "sync_session" => sync_session(d, args).await,
        "ls_skill" => ls_skill(d, args, origin.session()).await,
        "run_skill" => run_skill(d, args, origin.session()).await,
        "events" => events(d, args, origin.session()).await,
        "subscribe" => subscribe(d, args, origin.session()).await,
        "del_subscription" => del_subscription(d, args, origin.session()).await,
        "ls_subscription" => ls_subscription(d, args, origin.session()).await,
        "trigger_event" => trigger_event(d, args).await,
        // Every tool is a command: the registry decides what exists.
        name if d.agent.tools().get(name).is_some() => {
            crate::tool_cmd::dispatch_tool(d, name, args, origin.session()).await
        }
        // Published as a server command but unhandled: a catalog bug, named.
        other => Outcome::err(format!(
            "{other} is published as a server command but has no handler"
        )),
    }
}

/// Finds the command a call names, or says why it cannot run here.
///
/// # Invariant
///
/// Client commands are refused, not executed: the server cannot change a
/// caller's own state, and saying "unknown" would hide a catalog entry that is
/// right there in the greeting.
/// The one sentence a missing argument gets, so a rule enforced later reads the
/// same as one the catalog could state.
pub fn missing_arguments(name: &str, missing: &[String]) -> String {
    format!(
        "{name} is missing required argument(s): {}",
        missing.join(", ")
    )
}

pub fn resolve(catalog: &[CommandSpec], name: &str, args: &Args) -> Result<CommandSpec, Outcome> {
    let Some(spec) = catalog.iter().find(|c| c.name == name) else {
        return Err(Outcome::err(format!("unknown command {name:?}")));
    };
    if spec.target == CommandTarget::Client {
        return Err(Outcome::err(format!(
            "{name} runs in the client, not here: it changes the caller's own state"
        )));
    }
    let missing: Vec<String> = spec
        .args
        .iter()
        .filter(|a| a.required && !args.contains_key(&a.name))
        .map(|a| a.name.clone())
        .collect();
    if !missing.is_empty() {
        return Err(Outcome::err(missing_arguments(name, &missing)));
    }
    Ok(spec.clone())
}

impl Dispatch {
    /// Every command offered, from all four sources: the only list there is.
    pub async fn catalog(&self) -> Vec<CommandSpec> {
        let mut all = server_catalog(&self.sessions.remote_names());
        let tools = self.agent.tools();
        let scripts = self.scripts.commands().await;
        // A name this build implements is not a declaration's to take.
        let mut reserved: Vec<String> = all.iter().map(|spec| spec.name.clone()).collect();
        reserved.extend(scripts.iter().map(|spec| spec.name.clone()));
        reserved.extend(
            tools
                .iter()
                .filter(|tool| tool.forwards_to().is_none())
                .map(|tool| tool.name().to_string()),
        );
        let declared = self
            .declared
            .commands(&crate::content_commands::Publish {
                skills: tools.skills(),
                tools: &tools,
                reserved: &reserved,
            })
            .await;
        all.extend(declared);
        all.extend(crate::tool_cmd::catalog(&tools));
        all.extend(scripts);
        all
    }
}

async fn list_paths(d: &Dispatch, args: &Args) -> Outcome {
    let remote = args.word("remote");
    let path = args.word("path").unwrap_or("");
    match d.sessions.list_paths(remote, path).await {
        Ok(entries) => {
            let rows: Vec<serde_json::Value> = entries
                .into_iter()
                .map(|(name, directory)| json!({"name": name, "dir": directory}))
                .collect();
            let count = rows.len();
            Outcome::ok_with(
                format!("{count} entries in {path:?}"),
                json!({"path": path, "remote": remote, "entries": rows}),
            )
        }
        Err(e) => Outcome::err(e.to_string()),
    }
}

/// The session a command runs for: the one it names, else its caller's.
///
/// # Invariant
///
/// SPEC's `session: Session = caller()`: naming a session acts on that one, and omitting
/// it acts on the session the call was run from. See `docs/notes/server-commands.md`.
pub(crate) fn session_of(args: &Args, caller: Option<&str>) -> Option<String> {
    let named = args
        .word("session")
        .map(str::trim)
        .filter(|name| !name.is_empty());
    named
        .or_else(|| caller.map(str::trim).filter(|name| !name.is_empty()))
        .map(str::to_string)
}

/// The session a command acts on, or the sentence a call that has none gets.
///
/// # Invariant
///
/// SPEC's `session = caller()`: a call that names one acts on that one, and a call that
/// names none acts on the session it runs from. One that has neither -- a window with
/// no shell behind it, a signal -- is told so, rather than acting on the empty name.
/// See `docs/notes/server-commands.md`.
fn caller_session(name: &str, args: &Args, caller: Option<&str>) -> Result<String, Outcome> {
    match session_of(args, caller) {
        Some(session) => Ok(session),
        None => Err(Outcome::err(format!(
            "{name} acts on a session, and neither the call nor its client named one: \
             name one, or run it from a session's shell, where `$GONGGA_SESSION` is set"
        ))),
    }
}

async fn send_message(d: &Dispatch, args: &Args, caller: Option<&str>) -> Outcome {
    let Some(message) = args.word("message").map(str::to_string) else {
        return Outcome::err("send_message requires a message");
    };
    let Some(name) = session_of(args, caller) else {
        return Outcome::err(
            "send_message runs for a session: name one, or run it from a session's shell",
        );
    };
    deliver(d, &name, message).await
}

/// Resolves a message's `@image(...)` and `@skill(...)` references before it is stored.
///
/// # Invariant
///
/// A message is expanded once, so a stored row never carries a reference a later
/// retry would resolve differently. A session whose provider cannot be resolved still
/// takes its message: only the skills are expanded then.
async fn expand_message(
    d: &Dispatch,
    session: &crate::session::SessionHandle,
    message: String,
) -> Result<crate::meta::Expanded, String> {
    // Where this message's references are read: the machine the session stands on;
    // a remote with no connection says so.
    let reader = session.reader();
    let source = match (&reader, session.owner()) {
        (Some(reader), crate::session::Machine::Remote) => crate::meta::Source::Remote(reader),
        (None, crate::session::Machine::Remote) => crate::meta::Source::Unreachable,
        (_, crate::session::Machine::Local) => crate::meta::Source::Local,
    };
    let llm = d.agent.route(session.name(), &session.tuning()).ok();
    let llm = llm.as_ref().map(|(client, _, _)| client.as_ref());
    let home = session.cwd();
    Ok(crate::meta::expand(&d.store, source, &home, &message, llm).await)
}

/// Puts `message` into `name`'s hands: a turn when it is free, an inbox row when not.
///
/// # Invariant
///
/// The gate is claimed before the durable write, so a second sender can only
/// inject, never spawn a competing turn.
pub(crate) async fn deliver(d: &Dispatch, name: &str, message: String) -> Outcome {
    let session = match d.sessions.get(name) {
        Ok(s) => s,
        Err(e) => return Outcome::err(e.to_string()),
    };
    if session.is_deleted() {
        return Outcome::err(format!("session {name:?} is being deleted"));
    }
    let message = match expand_message(d, &session, message).await {
        Ok(message) => message,
        Err(e) => return Outcome::err(e),
    };
    // SPEC's `cancel` queues notices: a turn started in the stop window is cut by it.
    if d.shutting_down.load(Ordering::SeqCst) || d.shutdown.is_cancelled() {
        let mut payload = message.to_json();
        payload["session"] = json!(name);
        return match d.store.add_pending_task(&name, "inject", &payload) {
            Ok(_) => {
                tracing::info!(
                    session = %name,
                    "message queued for the next start: the server is stopping"
                );
                Outcome::ok(format!(
                    "message queued for {name}; the server is stopping, so the next start \
                 reads it"
                ))
            }
            Err(e) => Outcome::err(format!("could not record the pending message: {e}")),
        };
    }
    // SPEC: a work session opens its checkpoint before its first message; a dream opens none.
    if session.mode() == crate::explore::Mode::Work
        && session.dream_of().is_none()
        && let Err(e) = d.store.start_live_compact(session.id())
    {
        tracing::warn!(session = %name, error = %e, "could not open the live compact checkpoint");
    }
    let epoch = session.cancel_epoch();
    if let Some(gate) = session.try_claim() {
        // The gate is claimed before the durable write, so a second sender can only
        // inject, never spawn a competing turn.
        match d.store.begin_turn(session.id(), name, &message) {
            Ok((task, rows)) => {
                // Both rows are published before their position: the operator's line
                // and the plan notice behind it, if one landed.
                d.agent.announce_operator_rows(name, rows);
                spawn_turn(
                    d,
                    TurnWork {
                        session,
                        tasks: vec![task],
                        epoch,
                        gate,
                    },
                );
                // Which of the two a wait is: a turn started now, or an inbox row.
                tracing::info!(session = %name, "message accepted: the turn starts now");
                Outcome::ok(format!("message accepted by {name}"))
            }
            Err(e) => Outcome::err(format!("could not record the pending turn: {e}")),
        }
    } else {
        match d.store.add_pending_task(name, "inject", &{
            let mut payload = message.to_json();
            payload["session"] = json!(name);
            payload
        }) {
            Ok(_) => {
                session.note_message();
                tracing::info!(
                    session = %name,
                    "message queued: a running turn reads it at its next step boundary"
                );
                Outcome::ok(format!(
                    "message queued for {name}; the running command has been told to stop waiting"
                ))
            }
            Err(e) => Outcome::err(format!("could not record the pending message: {e}")),
        }
    }
}

/// Creates `name` the way `set_session` does, and answers why not when it will not.
///
/// # Invariant
///
/// A monitor's session comes into being here and nowhere else, so it starts on the
/// model, the tier and the tuning a session a person named starts on: this is
/// `set_session` with a name and nothing else, never a second creation path that a
/// later change to one of them would leave behind.
pub(crate) async fn create_session(d: &Dispatch, name: &str) -> Result<(), String> {
    let mut args = Args::new();
    args.insert("id", name.to_string());
    let outcome = set_session(d, &args).await;
    if outcome.ok {
        Ok(())
    } else {
        Err(outcome.message)
    }
}

async fn set_session(d: &Dispatch, args: &Args) -> Outcome {
    // SPEC's nested `config` flattens into the settings the flat flags name, so
    // one body applies them either way.
    let mut owned = args.clone();
    if let Some(raw) = owned.value("config").cloned() {
        if let Err(refusal) = crate::session_config::flatten(&raw, &mut owned) {
            return Outcome::err(refusal);
        }
    }
    let args = &owned;
    let name = args.word("id").unwrap_or_default().to_string();
    // Whether the name is a name at all: the list draws one row per prefix.
    if let Some(refusal) = crate::session::checked_name(&name) {
        return Outcome::err(refusal);
    }
    // SPEC's `Workspace.worktree`, spelled `none` for a session that makes none.
    let asked_worktree = crate::worktree::requested(args.word("worktree"));
    // A caller that means to start its own session says so: a name collision
    // must never select another one's transcript.
    match flag_of(args, "must_create") {
        Ok(Some(true)) if d.store.get_session(&name).ok().flatten().is_some() => {
            return Outcome::err(format!(
                "a session named {name} already exists, and --must_create refuses to join it; pick a name of your own",
            ));
        }
        Ok(_) => {}
        Err(e) => return Outcome::err(e),
    }
    // Resolve `--model` now: a typo should fail the command that made it.
    let stated = match args.word("model") {
        None => None,
        Some(model) => match resolve_model(d, model) {
            Ok(entry) => Some(entry),
            Err(e) => return Outcome::err(e),
        },
    };
    // Three sources, never the config's `[model]`: what this command names, what
    // the row already carries, then the constant.
    let entry = match stated {
        Some(entry) => entry,
        None => match d.sessions.recorded_model(&name) {
            Some(model) => match resolve_model(d, &model) {
                Ok(entry) => entry,
                Err(e) => return Outcome::err(e),
            },
            None => {
                let stated_default = crate::config::DEFAULT_SESSION_MODEL;
                if stated_default.is_empty() {
                    // The catalog's sentence for a missing argument: `--model`, needed
                    // only when nothing else names a model.
                    return Outcome::err(missing_arguments("set_session", &["model".to_string()]));
                }
                match resolve_model(d, stated_default) {
                    Ok(entry) => entry,
                    Err(e) => return Outcome::err(format!("DEFAULT_SESSION_MODEL: {e}")),
                }
            }
        },
    };
    let threshold = match args.word("ctx_limit") {
        None => None,
        Some(raw) => match parse_percent(raw) {
            Ok(v) => Some(v),
            Err(e) => return Outcome::err(e),
        },
    };
    // Absent, stated, stated-as-none: three answers, and only the last sends none.
    let cache_tier: Option<Option<String>> = match args.word("cache_tier") {
        None => None,
        Some(protocol::NONE_VALUE) => Some(None),
        Some(tier) => {
            if !entry.cache_ttl_tiers.contains_key(tier) {
                let mut known = entry.cache_ttl_tiers.keys().cloned().collect::<Vec<_>>();
                known.sort();
                known.push(protocol::NONE_VALUE.to_string());
                return Outcome::err(format!(
                    "--cache_tier {tier:?} is not a tier this model declares: {}",
                    known.join(", ")
                ));
            }
            Some(Some(tier.to_string()))
        }
    };
    let cache_keepalive = match flag_of(args, "keep_alive") {
        Ok(value) => value,
        Err(e) => return Outcome::err(format!("--keep_alive: {e}")),
    };
    // What this session may do to the workspace. Absent keeps what it had.
    let mode = match args.word("mode") {
        None => None,
        Some(raw) => match crate::explore::Mode::parse(raw) {
            Ok(mode) => Some(mode),
            Err(e) => return Outcome::err(format!("--mode: {e}")),
        },
    };
    let live_enabled = match flag_of(args, "live_compact_enabled") {
        Ok(value) => value,
        Err(e) => return Outcome::err(format!("--live_compact_enabled: {e}")),
    };
    // Zero would ask at every step; the interval is the threshold, not the switch.
    let live_interval = match count_of(args, "live_compact_interval_tokens") {
        Ok(None) => None,
        Ok(Some(tokens)) if tokens > 0 => Some(tokens as u64),
        Ok(Some(_)) => {
            return Outcome::err(
                "--live_compact_interval_tokens takes a positive count of tokens: the                  interval is the threshold, not the switch"
                    .to_string(),
            );
        }
        Err(e) => return Outcome::err(format!("--live_compact_interval_tokens: {e}")),
    };
    // A percentage needs a window; silently keeping the old budget would lie.
    let context = entry.context.or(d.agent.config.now().model.context);
    if let (Some(percent), None) = (threshold, context) {
        return Outcome::err(format!(
            "--ctx_limit {percent}% needs a context window to be a percentage of, and \
         {} is not in the roster with a `context`",
            entry.model
        ));
    }
    // A session that exists is moved; one that does not is created where it says.
    // Asking where it already is costs nothing.
    let stated_cwd = args.word("cwd");
    // Three answers, not two: the flag was left out, it names this machine, or it
    // names a remote. See `session::RemoteAsk`.
    let asked_remote = match args.word("remote") {
        None => crate::session::RemoteAsk::Keep,
        Some(stated) => crate::session::RemoteAsk::Machine(crate::session::remote_request(stated)),
    };
    let (session, moved) = match d.store.get_session(&name).ok().flatten() {
        Some(row) => {
            if let Some(asked) = asked_worktree {
                let recorded = row.worktree.as_deref();
                if recorded != Some(asked) {
                    return Outcome::err(format!(
                        "session {name} was created with {}, and nothing here changes that: a \
                         worktree is made with the session that asked for it",
                        match recorded {
                            Some(worktree) => format!("the worktree {worktree:?}"),
                            None => "no worktree".to_string(),
                        }
                    ));
                }
            }
            let live = d.sessions.peek(&name);
            let here = live
                .as_ref()
                .map(|session| session.cwd())
                .unwrap_or_else(|| std::path::PathBuf::from(&row.cwd));
            let recorded_remote = row.remote.clone();
            let (remote_moves, lands) = match &asked_remote {
                crate::session::RemoteAsk::Keep => (false, recorded_remote.clone()),
                crate::session::RemoteAsk::Machine(named) => {
                    (named != &recorded_remote, named.clone())
                }
            };
            // Read from the destination's side: the session's directory here,
            // the server's on arrival, and the peer's when it moves.
            let to_here = lands.is_none();
            let base = if remote_moves {
                std::env::current_dir().unwrap_or_default()
            } else {
                here.clone()
            };
            let wanted_dir = stated_cwd.map(|asked| match to_here {
                true => crate::session::stated_directory(asked, &base),
                false => std::path::PathBuf::from(asked.trim()),
            });
            let cwd_moves = wanted_dir
                .as_deref()
                .is_some_and(|asked| !same_directory(asked, &here));
            if let Some(worktree) = &row.worktree
                && (remote_moves || cwd_moves)
            {
                return Outcome::err(format!(
                    "session {name} works in the worktree {worktree:?}, which is made with it \
                     and removed with it: it can be neither moved nor left on its own"
                ));
            }
            let session = match live {
                Some(session) => session,
                None => match d.sessions.get(&name) {
                    Ok(session) => session,
                    Err(e) => return Outcome::err(e.to_string()),
                },
            };
            if !remote_moves && !cwd_moves {
                (session, None)
            } else {
                if let Some(directory) = wanted_dir.as_deref() {
                    if let Err(e) = d
                        .sessions
                        .check_placement(lands.as_deref(), directory)
                        .await
                    {
                        return Outcome::err(e.to_string());
                    }
                }
                let stated = wanted_dir
                    .as_ref()
                    .map(|dir| dir.to_string_lossy().into_owned());
                let moved = match d
                    .sessions
                    .relocate(&session, asked_remote.clone(), stated.as_deref())
                    .await
                {
                    Ok(report) => report,
                    Err(e) => return Outcome::err(e.to_string()),
                };
                (session, Some(moved))
            }
        }
        None => {
            // Naming a session creates it: that is the only way to get one.
            let lands = match &asked_remote {
                crate::session::RemoteAsk::Keep => None,
                crate::session::RemoteAsk::Machine(named) => named.clone(),
            };
            // SPEC's `Workspace.worktree`: made in the session's own space, and before
            // its row, so a refusal leaves nothing behind.
            let space = match asked_worktree {
                None => None,
                Some(_) => match d.sessions.space_on(lands.as_deref()) {
                    Ok(space) => Some(space),
                    Err(e) => return Outcome::err(e.to_string()),
                },
            };
            let born = match (asked_worktree, &space) {
                (Some(asked), Some(space)) => {
                    let start = started_at(stated_cwd, space.is_local());
                    match make_worktree(space, &start, asked).await {
                        Ok(path) => Some((asked.to_string(), path)),
                        Err(e) => return Outcome::err(e),
                    }
                }
                _ => None,
            };
            if born.is_none()
                && let Some(stated) = stated_cwd
            {
                let directory = crate::session::placed_directory(
                    stated,
                    lands.as_deref(),
                    &std::env::current_dir().unwrap_or_default(),
                );
                if let Err(e) = d
                    .sessions
                    .check_placement(lands.as_deref(), &directory)
                    .await
                {
                    return Outcome::err(e.to_string());
                }
            }
            // A session born in a worktree works at its root: what it stated is the
            // checkout the branch was made in.
            let directory = born
                .as_ref()
                .map(|(_, path)| path.to_string_lossy().into_owned());
            match d.sessions.ensure_on(
                &name,
                directory.as_deref().or(stated_cwd),
                Some(&entry.model),
                lands.as_deref(),
                asked_worktree,
            ) {
                Ok(session) => (session, None),
                Err(e) => {
                    if let (Some((worktree, path)), Some(space)) = (&born, &space)
                        && let Err(left) = remove_worktree(space, worktree, path).await
                    {
                        tracing::warn!(
                            session = %name, worktree = %worktree, error = %left,
                            "a refused session's worktree was left behind"
                        );
                    }
                    return Outcome::err(e.to_string());
                }
            }
        }
    };

    let mut tuning = session.tuning();
    let mut changed = false;
    // Resolved here and stored, never inferred at display time, so the title and
    // the request read one word.
    let level = match args.word("thinking") {
        // Names no level: clears one earlier, rather than keeping it.
        Some(protocol::THINKING_DEFAULT) => entry.level_in_force(None),
        // Checked against the model this session runs on, so typos fail now.
        Some(raw) => match entry.reasoning_effort(raw) {
            Ok(level) => Some(level),
            Err(message) => return Outcome::err(message),
        },
        None => entry.level_in_force(tuning.thinking_effort.as_deref()),
    };
    if tuning.thinking_effort != level {
        tuning.thinking_effort = level;
        changed = true;
    }
    // Writing the session's own model is what keeps the config's out of its row.
    if tuning.model.as_deref() != Some(entry.model.as_str()) {
        tuning.model = Some(entry.model.clone());
        changed = true;
    }
    if let Some(percent) = threshold {
        tuning.context_limit = Some(percent);
        changed = true;
    }
    let cache_changed = cache_keepalive.is_some() || cache_tier.is_some();
    if let Some(value) = cache_keepalive {
        tuning.cache_keepalive = value;
    }
    if let Some(tier) = cache_tier {
        tuning.cache_tier = tier;
    }
    let live_changed = live_enabled.is_some() || live_interval.is_some();
    if let Some(value) = live_enabled {
        tuning.live_compact_enabled = Some(value);
    }
    if let Some(tokens) = live_interval {
        tuning.live_compact_interval_tokens = Some(tokens);
    }
    // Zero is not a threshold: enabled live compact needs a positive interval.
    let config = d.agent.config.now();
    let live_on = tuning
        .live_compact_enabled
        .unwrap_or(config.agent.live_compact_enabled);
    let live_interval_now = tuning
        .live_compact_interval_tokens
        .unwrap_or(config.agent.live_compact_interval_tokens);
    if live_on && live_interval_now == 0 {
        return Outcome::err(
            "--live_compact_interval_tokens is the threshold, not the switch: enable live \
             compact only with a positive interval"
                .to_string(),
        );
    }
    if changed || cache_changed || live_changed {
        // Persist before applying: a restart must restore what turns here read.
        if let Err(e) = d.store.set_session_tuning(
            session.id(),
            Some(&entry.model),
            tuning.context_limit,
            tuning.thinking_effort.as_deref(),
            &entry.model,
        ) {
            return Outcome::err(format!("could not save the session settings: {e}"));
        }
        if cache_changed {
            if let Err(e) = d.store.set_session_cache(
                session.id(),
                tuning.cache_tier.as_deref(),
                tuning.cache_keepalive,
            ) {
                return Outcome::err(format!("could not save the cache settings: {e}"));
            }
        }
        if live_changed {
            if let Err(e) = d.store.set_session_live_compact(
                session.id(),
                &name,
                tuning.live_compact_enabled,
                tuning.live_compact_interval_tokens,
            ) {
                return Outcome::err(format!("could not save the live compact settings: {e}"));
            }
        }
        session.set_tuning(tuning.clone());
    }
    // A mode that changed says so: the agent hears it at the next step boundary.
    if let Some(mode) = mode.filter(|mode| *mode != session.mode()) {
        // Plan finish (explore -> work) opens the starting checkpoint.
        if mode == crate::explore::Mode::Work
            && session.mode() == crate::explore::Mode::Explore
            && let Err(e) = d.store.start_live_compact(session.id())
        {
            return Outcome::err(format!("could not open the live compact checkpoint: {e:#}"));
        }
        if let Err(e) = d.store.set_session_mode(session.id(), mode.as_str()) {
            return Outcome::err(format!("could not save the session's mode: {e}"));
        }
        session.set_mode(mode);
        crate::explore::prime(session.explore(), session.cwd());
        if let Err(e) = d.store.add_pending_task(
            &name,
            "inject",
            &json!({
                "session": name,
                "message": crate::explore::mode_notice(crate::prompts::shared(), mode),
                "notice": "explore",
            }),
        ) {
            tracing::warn!(session = %name, error = %e, "could not say the session's mode");
        }
    }
    publish_sessions(d);

    let cwd = session.cwd();
    // Confirms the write and stops: what the session runs on is its pane's title.
    let message = match &moved {
        None => format!("session is now {name}"),
        // Waiting for the turn answers the one caller that wants a move: the
        // agent, in the turn it is moving out of.
        Some(crate::session::MoveOutcome::Queued(request)) => format!(
            "session {name} will move to {} {} when its turn ends",
            request
                .remote
                .clone()
                .unwrap_or_else(|| "this machine".to_string()),
            match &request.cwd {
                Some(path) => format!("at {path}"),
                None => "where that endpoint starts".to_string(),
            }
        ),
        Some(crate::session::MoveOutcome::Applied(report)) => {
            // Silence is not stopped: a shell whose end could not be proved is said
            // so here, because nothing later will mention it.
            let unproved = match report.unconfirmed.len() {
                0 => String::new(),
                n => format!("; {n} shell(s) may still be running on the machine it left"),
            };
            // A moved remote shell has not connected yet: no directory to name,
            // and "at" and nothing would read as a failure.
            let where_now = if cwd.as_os_str().is_empty() {
                "where its endpoint starts".to_string()
            } else {
                format!("at {}", cwd.display())
            };
            format!(
                "session {name} moved to {} {where_now}, {} shell(s) ended{unproved}",
                d.sessions
                    .recorded_remote(&name)
                    .unwrap_or_else(|| "this machine".to_string()),
                report.ended
            )
        }
    };
    // A queued move has not happened, so the answer carries where the session is,
    // not where it was asked to go.
    let queued = matches!(moved, Some(crate::session::MoveOutcome::Queued(_)));
    Outcome::ok_with(
        message,
        json!({
            "session": name,
            "move_pending": queued,
            "cwd": cwd,
            "remote": d.sessions.recorded_remote(&name),
            "model": tuning.model,
            "context_limit": tuning.context_limit,
            "thinking_effort": tuning.thinking_effort,
            "mode": session.mode().as_str(),
        }),
    )
}

/// Whether two spellings name one directory, so a `--cwd` that agrees is not a move.
///
/// # Invariant
///
/// Comparing paths as strings would refuse `C:/work` against `C:\\work`, which name
/// one directory; this compares what the platform compares.
fn same_directory(spelled: &std::path::Path, actual: &std::path::Path) -> bool {
    if spelled == actual {
        return true;
    }
    // A canonicalising comparison answers "no" for a path that does not exist;
    // this flattens what the platform ignores.
    fn flatten(path: &std::path::Path) -> String {
        let text = path
            .to_string_lossy()
            .replace(['/', '\\'], std::path::MAIN_SEPARATOR_STR)
            .trim_end_matches(std::path::MAIN_SEPARATOR)
            .to_string();
        if cfg!(windows) {
            text.to_lowercase()
        } else {
            text
        }
    }
    flatten(spelled) == flatten(actual)
}

/// `--model` to a roster entry: the pair first, then a bare name.
fn resolve_model(d: &Dispatch, model: &str) -> Result<crate::providers::Entry, String> {
    match d.providers.resolve(model) {
        Ok(choice) => Ok(choice.entry),
        // A bare name is not an error the roster answers twice: the second
        // search runs only when the first found no pair.
        Err(e) => d
            .providers
            .resolve_by_name(model)
            .map(|c| c.entry)
            .ok_or_else(|| format!("model {model}: {e}")),
    }
}

/// One boolean argument: a JSON `true`, or the word a script's own line spells.
///
/// # Invariant
///
/// A boolean is a boolean on the wire; the word is read only because a script's line
/// spells its arguments as text. A stated `null` is the empty value of the type.
fn flag_of(args: &Args, key: &str) -> Result<Option<bool>, String> {
    match args.given(key) {
        protocol::Given::Nothing => Ok(None),
        protocol::Given::Null => Ok(Some(false)),
        protocol::Given::Value(value) => match (value.as_bool(), value.as_str()) {
            (Some(flag), _) => Ok(Some(flag)),
            (None, Some(word)) => parse_bool(word).map(Some),
            (None, None) => Err(format!("`{key}` takes a boolean, and this is {value}")),
        },
    }
}

/// One count argument: a JSON number, or the digits a script's own line spells.
fn count_of(args: &Args, key: &str) -> Result<Option<i64>, String> {
    match args.given(key) {
        protocol::Given::Nothing => Ok(None),
        protocol::Given::Null => Ok(Some(0)),
        protocol::Given::Value(value) => match (value.as_i64(), value.as_str()) {
            (Some(count), _) => Ok(Some(count)),
            (None, Some(word)) => word
                .trim()
                .parse::<i64>()
                .map(Some)
                .map_err(|_| format!("`{key}` takes a number, and {word:?} is not one")),
            (None, None) => Err(format!("`{key}` takes a number, and this is {value}")),
        },
    }
}

fn parse_bool(raw: &str) -> Result<bool, String> {
    match raw.trim() {
        "true" => Ok(true),
        "false" => Ok(false),
        other => Err(format!(
            "expected {}, not {other:?}",
            protocol::SHOW_VALUES.join(" or ")
        )),
    }
}

/// `30%`, `30`, or `0` for "never notice"; anything else is refused, not rounded.
fn parse_percent(raw: &str) -> Result<u8, String> {
    let trimmed = raw.trim().trim_end_matches('%').trim();
    let value: u16 = trimmed
        .parse()
        .map_err(|_| format!("--ctx_limit takes a percentage like `30%`, got {raw:?}"))?;
    if value > 100 {
        return Err(format!(
            "--ctx_limit is a percentage of the context window, so it cannot exceed 100%; \
         got {raw}"
        ));
    }
    Ok(value as u8)
}

/// An inclusive window of `seq` numbers, `first > last` meaning empty.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct SeqWindow {
    first: i64,
    last: i64,
}

/// A slice over sequence numbers; see `docs/notes/server-commands.md`.
fn parse_seq_slice(raw: &str, last_seq: i64) -> Result<SeqWindow, String> {
    let one = |part: &str, side: &str| -> Result<i64, String> {
        let part = part.trim();
        let n: i64 = part.parse().map_err(|_| {
            format!("range takes sequence numbers like `5:10` or `-5:`, got {part:?} in the {side}")
        })?;
        // Seq starts at 1, so 0 is a typo written either way.
        if n == 0 {
            return Err(format!(
                "range counts sequence numbers from 1, or from -1 backwards; got 0 in the {side}"
            ));
        }
        // Negative counts back from the tail, like a Python index: -1 is the newest row.
        Ok(if n > 0 {
            n
        } else {
            last_seq.saturating_add(n).saturating_add(1)
        })
    };

    let raw = raw.trim();
    // A second colon is a step (`1:2:3`), which names no range here.
    if raw.matches(':').count() > 1 {
        return Err(format!(
            "range takes one `start:end` range, not a step: got {raw:?}"
        ));
    }
    let window = match raw.split_once(':') {
        None => {
            let at = one(raw, "index")?;
            SeqWindow {
                first: at,
                last: at,
            }
        }
        Some((start, end)) => {
            if start.trim().is_empty() && end.trim().is_empty() {
                return Err("range needs a bound on one side, e.g. `-5:` or `:10`".into());
            }
            let first = if start.trim().is_empty() {
                1
            } else {
                one(start, "start")?
            };
            let last = if end.trim().is_empty() {
                last_seq
            } else {
                one(end, "end")?
            };
            // Backwards as written is a typo; empty after clamping is an answer.
            if !start.trim().is_empty() && !end.trim().is_empty() && first > last {
                return Err(format!(
                    "range {raw} runs backwards: it starts at {first} and ends at {last}"
                ));
            }
            SeqWindow { first, last }
        }
    };

    // Clamp like a slice: asking for more than exists is fewer rows, not an error.
    let first = window.first.max(1);
    let last = window.last.min(last_seq);
    Ok(SeqWindow { first, last })
}

/// Whether `name` is `path` itself or below it.
///
/// # Invariant
///
/// A path is a component, not a string prefix: `a/b` is under `a`, and `ab` is not.
/// Empty is every path, which is what "list them all" and "delete them all" mean.
fn under(name: &str, path: &str) -> bool {
    let path = path.trim_end_matches('/');
    path.is_empty()
        || name == path
        || name
            .strip_prefix(path)
            .is_some_and(|rest| rest.starts_with('/'))
}

async fn ls_channel(d: &Dispatch, args: &Args, caller: Option<&str>) -> Outcome {
    let session = match caller_session("ls_channel", args, caller) {
        Ok(session) => session,
        Err(refusal) => return refusal,
    };
    let prefix = args.word("path").map(str::to_string).unwrap_or_default();
    match d.agent.channels.list(&session).await {
        Ok(rows) => {
            let listed: Vec<serde_json::Value> = rows
                .into_iter()
                .filter(|row| row["id"].as_str().is_some_and(|id| under(id, &prefix)))
                .collect();
            if listed.is_empty() {
                return Outcome::ok(format!("{session}: no channel matches {prefix:?}"));
            }
            Outcome::ok(serde_json::Value::Array(listed).to_string())
        }
        Err(why) => Outcome::err(why),
    }
}

async fn del_channel(d: &Dispatch, args: &Args, caller: Option<&str>) -> Outcome {
    let session = match caller_session("del_channel", args, caller) {
        Ok(session) => session,
        Err(refusal) => return refusal,
    };
    let id = args.word("id").unwrap_or_default().to_string();
    // SPEC: a path takes what is under it, so a name in the middle of one ends
    // every channel below it as well.
    let doomed: Vec<String> = match d.agent.channels.list(&session).await {
        Ok(rows) => rows
            .iter()
            .filter_map(|row| row["id"].as_str())
            .filter(|path| under(path, &id))
            .map(str::to_string)
            .collect(),
        Err(why) => return Outcome::err(why),
    };
    if doomed.len() > 1 {
        let mut ended = 0;
        for path in &doomed {
            if let Ok(true) = d.agent.channels.stop(&session, path).await {
                ended += 1;
            }
        }
        return Outcome::ok(format!(
            "ended {ended} channel(s) under {id:?}: {}",
            doomed.join(", ")
        ));
    }
    match d.agent.channels.stop(&session, &id).await {
        Ok(true) => Outcome::ok(format!("ended channel {id:?}")),
        Ok(false) => Outcome::ok(format!("channel {id:?} had already ended; forgotten")),
        Err(why) => Outcome::err(why),
    }
}

async fn rename_channel(d: &Dispatch, args: &Args, caller: Option<&str>) -> Outcome {
    let session = match caller_session("rename_channel", args, caller) {
        Ok(session) => session,
        Err(refusal) => return refusal,
    };
    let path = args.word("id").unwrap_or_default().to_string();
    let new_path = args.word("new_id").unwrap_or_default().to_string();
    match d.agent.channels.rename(&session, &path, &new_path) {
        Ok(()) => Outcome::ok(format!("{path:?} is now {new_path:?}")),
        Err(why) => Outcome::err(why),
    }
}

async fn new_channel(d: &Dispatch, args: &Args, caller: Option<&str>) -> Outcome {
    let session = match caller_session("new_channel", args, caller) {
        Ok(session) => session,
        Err(refusal) => return refusal,
    };
    let id = args.word("id").unwrap_or_default().to_string();
    let name = args.word("name").unwrap_or_default().to_string();
    let handle = match d.sessions.get(&session) {
        Ok(handle) => handle,
        Err(e) => return Outcome::err(e.to_string()),
    };
    match d
        .agent
        .channels
        .start(
            &session,
            &name,
            &[],
            handle.remote_name().as_deref(),
            &handle.cwd(),
            &handle.env(),
            &id,
        )
        .await
    {
        Ok(row) => Outcome::ok_with(
            format!("channel {id:?} started: {name}, pid {}", row.pid),
            json!({"id": id, "session": session, "program": name}),
        ),
        Err(why) => Outcome::err(why),
    }
}

async fn input_channel(d: &Dispatch, args: &Args, caller: Option<&str>) -> Outcome {
    let session = match caller_session("input_channel", args, caller) {
        Ok(session) => session,
        Err(refusal) => return refusal,
    };
    let id = args.word("id").unwrap_or_default().to_string();
    let input = args.word("input").unwrap_or_default().to_string();
    let wait = match count_of(args, "wait") {
        Ok(None) => std::time::Duration::from_millis(30_000),
        Ok(Some(ms)) if ms >= 0 => std::time::Duration::from_millis(ms as u64),
        Ok(Some(ms)) => return Outcome::err(format!("--wait takes milliseconds, not {ms}")),
        Err(e) => return Outcome::err(e),
    };
    let until = args.word("until");
    let began = std::time::Instant::now();
    match d
        .agent
        .channels
        .talk(&session, &id, Some(&input), wait, until, None)
        .await
    {
        Ok(outcome) => Outcome::ok_with(
            outcome.output.clone(),
            json!({
                "id": outcome.id,
                "output": outcome.output,
                "time": format!("{:.3}s", began.elapsed().as_secs_f64()),
                "matched": outcome.matched,
                "exit": outcome.exit,
            }),
        ),
        Err(why) => Outcome::err(why),
    }
}

async fn ls_session(d: &Dispatch, args: &Args) -> Outcome {
    let prefix = args.word("path").map(str::to_string).unwrap_or_default();
    let rows = match d.store.list_sessions() {
        Ok(rows) => rows,
        Err(e) => return Outcome::err(format!("could not read the sessions: {e}")),
    };
    let listed: Vec<serde_json::Value> = rows
        .into_iter()
        .filter(|row| under(&row.name, &prefix))
        .map(|row| {
            json!({
                "name": row.name,
                "status": format!("{:?}", row.status),
                "model": row.model,
                "cwd": row.cwd,
                "context_limit": row.context_limit,
                "thinking_effort": row.thinking_effort,
                "created_at": row.created_at,
                "updated_at": row.updated_at,
            })
        })
        .collect();
    if listed.is_empty() {
        return Outcome::ok(format!("no session matches {prefix:?}"));
    }
    Outcome::ok(serde_json::Value::Array(listed).to_string())
}

async fn ls_subscription(d: &Dispatch, args: &Args, caller: Option<&str>) -> Outcome {
    let session = match caller_session("ls_subscription", args, caller) {
        Ok(session) => session,
        Err(refusal) => return refusal,
    };
    let prefix = args.word("path").map(str::to_string).unwrap_or_default();
    let rows = match d.store.subscriptions() {
        Ok(rows) => rows,
        Err(e) => return Outcome::err(format!("could not read the subscriptions: {e}")),
    };
    let listed: Vec<serde_json::Value> = rows
        .into_iter()
        .filter(|row| row.session == session && under(&row.name, &prefix))
        .map(|row| {
            json!({
                "subscription": row.name,
                "pattern": row.pattern,
                "persistant": row.persistent,
                "action": row.action,
            })
        })
        .collect();
    if listed.is_empty() {
        return Outcome::ok(format!("{session}: no subscription matches {prefix:?}"));
    }
    Outcome::ok(serde_json::Value::Array(listed).to_string())
}

async fn get_messages(d: &Dispatch, args: &Args, caller: Option<&str>) -> Outcome {
    let name = match caller_session("get_messages", args, caller) {
        Ok(name) => name,
        Err(refusal) => return refusal,
    };
    let session = match d.sessions.get(&name) {
        Ok(s) => s,
        Err(e) => return Outcome::err(e.to_string()),
    };
    // The one boolean reader: a JSON `true`, or the word a script's line spells.
    let replay = match flag_of(args, "replay") {
        Ok(flags) => flags.unwrap_or(false),
        Err(e) => return Outcome::err(format!("--replay: {e}")),
    };
    if replay {
        return replay_transcript(d, &name, &session, args);
    }
    let last_seq = match d.store.last_seq(session.id()) {
        Ok(v) => v,
        Err(e) => return Outcome::err(e.to_string()),
    };
    let window = match args.word("range") {
        None => SeqWindow {
            first: 1,
            last: last_seq.max(1),
        },
        Some(raw) => match parse_seq_slice(raw, last_seq) {
            Ok(w) => w,
            Err(e) => return Outcome::err(e),
        },
    };
    // No size judgement: the channel says so if the answer cannot be carried.
    let t = match d
        .store
        .messages_between(session.id(), window.first, window.last)
    {
        Ok(t) => t,
        Err(e) => return Outcome::err(e.to_string()),
    };
    transcript_outcome(&name, t)
}

/// The transcript as a window draws it, plus the answer still being written.
///
/// # Invariant
///
/// A snapshot with its own stream position: rows, streaming parts and number are read
/// inside the bus's gate, so a client can tell which events it receives are already
/// here (`seq <= as_of`) and which are news. Without the number a refill repeats or
/// loses the tail of an answer.
///
/// # Invariant
///
/// Only what a window draws: a notice is the server's own line and is skipped, and the
/// operator's messages come back as [`ResponsePart::Input`].
fn replay_transcript(
    d: &Dispatch,
    name: &str,
    session: &crate::session::SessionHandle,
    args: &Args,
) -> Outcome {
    // Read inside the gate, the end included: a later row's event is numbered above.
    let (as_of, read) = d.bus.as_of(|| {
        let last = d.store.last_seq(session.id())?;
        let window = match args.word("range") {
            None => SeqWindow {
                first: 1,
                last: last.max(1),
            },
            // A range this build cannot read is a caller's mistake: refused here
            // rather than reported as a store failure.
            Some(raw) => parse_seq_slice(raw, last).map_err(anyhow::Error::msg)?,
        };
        let rows = d
            .store
            .messages_between(session.id(), window.first, window.last)?;
        let through = window.last.min(last);
        let open = if through == last {
            session.open_parts()
        } else {
            Vec::new()
        };
        Ok::<_, anyhow::Error>((rows, open, window.first, through))
    });
    let (read, open, first, through) = match read {
        Ok(t) => t,
        Err(e) => return Outcome::err(e.to_string()),
    };
    let mut events: Vec<(Option<u64>, Event)> = read
        .messages
        .iter()
        .filter_map(|row| {
            row_event(name, row, true).map(|event| (Some(row.seq.max(0) as u64), event))
        })
        .collect();
    // Published and not yet stored: what a window that reconnected has no other copy of.
    for part in &open {
        if part.text.trim().is_empty() {
            continue;
        }
        events.push((
            None,
            Event::Response(ResponseEvent {
                session: name.to_string(),
                turn: part.turn,
                part: part.part,
                text: part.text.clone(),
                done: false,
                call_id: part.call_id.clone(),
            }),
        ));
    }
    let count = events.len();
    let message = format!(
        "{count} event(s) at seq {first}..{} in {name} (transcript ends at seq {}, stream at {as_of})",
        read.last_seq, read.last_seq
    );
    Outcome::ok_with(
        message,
        json!({
            "session": name,
            "status": read.status,
            "last_seq": read.last_seq,
            "first_seq": first,
            "through_seq": through,
            "as_of": as_of,
            "replayed": true,
        }),
    )
    .with_events(events)
}

/// Copies one session's transcript into a free name, and tells every client.
///
/// # Invariant
///
/// A client's view of the copy is its view of the source: the session list is published
/// first, then the same rows as the events they streamed as.
/// See `docs/notes/server-commands.md`.
async fn clone_session(d: &Dispatch, args: &Args) -> Outcome {
    let source = args.word("id").unwrap_or_default().to_string();
    let name = args.word("new_id").unwrap_or_default().to_string();
    let report = match d.store.clone_session(&source, &name, None) {
        Ok(report) => report,
        Err(e) => return Outcome::err(e.to_string()),
    };
    // List, then rows: what a copy written behind the server's back cannot send. See
    // `docs/notes/server-commands.md`.
    publish_sessions(d);
    match d.store.messages_between(report.id, 1, report.last_seq) {
        Ok(transcript) => {
            for row in &transcript.messages {
                if let Some(event) = row_event(&name, row, false) {
                    d.bus.publish(event);
                }
            }
        }
        // The copy is already durable: a failed re-send is a report, not an undo.
        Err(e) => d.bus.publish(Event::Log {
            level: LogLevel::Warn,
            message: format!("{name} was cloned, but its rows were not re-sent: {e}"),
            session: None,
        }),
    }
    tracing::info!(session = %name, source = %source, rows = report.rows, "session cloned");
    Outcome::ok_with(
        format!(
            "cloned {} row(s) of {source} into {name} (transcript ends at seq {})",
            report.rows, report.last_seq
        ),
        json!({
            "session": source,
            "name": name,
            "rows": report.rows,
            "last_seq": report.last_seq,
        }),
    )
}

/// Re-sends a session's state as deltas after its rows were written directly.
///
/// # Invariant
///
/// The rows are already durable: this only publishes what a window needs to see them --
/// the same events a clone or a live turn sends -- so a failed re-send is a report, not an
/// undo. The transcript stamp goes last, and a stamp that moved backwards is a cut: a
/// window forgets what it drew and refills. See `docs/notes/server-commands.md`.
async fn sync_session(d: &Dispatch, args: &Args) -> Outcome {
    let name = args.word("id").unwrap_or_default().to_string();
    let session = match d.sessions.get(&name) {
        Ok(s) => s,
        Err(e) => return Outcome::err(e.to_string()),
    };
    let last_seq = match d.store.last_seq(session.id()) {
        Ok(v) => v,
        Err(e) => return Outcome::err(e.to_string()),
    };
    let window = match args.word("trajectory") {
        None => SeqWindow {
            first: 1,
            last: last_seq.max(1),
        },
        Some(raw) => match parse_seq_slice(raw, last_seq) {
            Ok(w) => w,
            Err(e) => return Outcome::err(e),
        },
    };
    let transcript = match d
        .store
        .messages_between(session.id(), window.first, window.last)
    {
        Ok(t) => t,
        Err(e) => return Outcome::err(e.to_string()),
    };
    // List, then rows, then the stamp: what a write behind the server's back cannot send.
    publish_sessions(d);
    let mut sent = 0usize;
    for row in &transcript.messages {
        if let Some(event) = row_event(&name, row, false) {
            d.bus.publish(event);
            sent += 1;
        }
    }
    d.bus.publish(Event::Transcript {
        session: name.clone(),
        seq: last_seq,
    });
    tracing::info!(session = %name, rows = sent, last_seq, "session synced");
    Outcome::ok_with(
        format!("synced {name}: re-sent {sent} row(s), transcript ends at seq {last_seq}"),
        json!({
            "session": name,
            "rows": sent,
            "last_seq": last_seq,
        }),
    )
}

/// Every skill this session's roots hold, under `path`.
///
/// See `docs/notes/server-commands.md`.
///
/// # Invariant
///
/// One walk: the same `installed_for` that `run_skill` and `@skill(...)` read, so a
/// name this lists is a name `run_skill` takes. `path` is a bucket's own word or a
/// skill's name, matched by component (`under`): `harness` is every skill the home
/// installed, `har` is nothing, and a nested skill answers to the directories under it.
async fn ls_skill(d: &Dispatch, args: &Args, caller: Option<&str>) -> Outcome {
    let session = match caller_session("ls_skill", args, caller) {
        Ok(session) => session,
        Err(refusal) => return refusal,
    };
    let path = args.word("path").unwrap_or("").trim().to_string();
    let handle = match d.sessions.get(&session) {
        Ok(handle) => handle,
        Err(e) => return Outcome::err(e.to_string()),
    };
    let installed = crate::tools::skill::installed_for(&handle.cwd(), handle.owner());
    let listed: Vec<serde_json::Value> = installed
        .iter()
        .filter(|skill| under(&skill.id(), &path) || under(skill.bucket.as_str(), &path))
        .map(crate::tools::skill::row)
        .collect();
    if listed.is_empty() {
        return Outcome::ok(format!("{session}: no skill matches {path:?}"));
    }
    Outcome::ok(serde_json::Value::Array(listed).to_string())
}

/// Runs one skill as `session`: a package's request on its stdin.
///
/// # Invariant
///
/// SPEC's `## Python Cmd`, and the one runner there is: the id is resolved here, because
/// a name two roots spell is two skills, and what a skill *is* is decided in
/// one place, [`skill::Skills::run`]. The name answers with the most specific row, the
/// same one `@skill(...)` reads.
async fn run_skill(d: &Dispatch, args: &Args, caller: Option<&str>) -> Outcome {
    let session = match caller_session("run_skill", args, caller) {
        Ok(session) => session,
        Err(refusal) => return refusal,
    };
    let id = args.word("id").unwrap_or_default().trim().to_string();
    let cmd = args.value("cmd");
    let handle = match d.sessions.get(&session) {
        Ok(handle) => handle,
        Err(e) => return Outcome::err(e.to_string()),
    };
    let roots = crate::tools::skill::session_roots(&handle.cwd(), handle.owner());
    // One resolver, and the walk behind it is `ls_skill`'s.
    let Some(skill) = roots.at(&id) else {
        let known: Vec<String> = roots.all().iter().map(|skill| skill.id()).collect();
        return Outcome::err(format!(
            "no skill is {id:?} here; `ls_skill` lists {}",
            if known.is_empty() {
                "none".to_string()
            } else {
                known.join(", ")
            }
        ));
    };
    let table = d.agent.tools();
    let Some(skills) = table.skills() else {
        return Outcome::err(
            "run_skill is not carried: this build was given no skill programs".to_string(),
        );
    };
    let ctx = match crate::tool_cmd::context(&d.agent, &d.sessions, &d.store, &session, "run_skill")
    {
        Ok(ctx) => ctx,
        Err(why) => return Outcome::err(why),
    };
    let outcome = skills.run(&skill, cmd, &ctx).await;
    match outcome.ok {
        true => Outcome::ok(outcome.output),
        false => Outcome::err(outcome.output),
    }
}

/// Runs one command the content declares: the skill's function, as the caller's session.
///
/// # Invariant
///
/// One implementation: `run_skill` is the door a package skill is opened by, so a declared
/// command is that call under a shorter name -- `id` is the skill, `cmd` the function and
/// the arguments this call carried. Nothing here knows a skill by any other route.
async fn declared_command(
    d: &Dispatch,
    route: &crate::content_commands::Route,
    args: &Args,
    caller: Option<&str>,
) -> Outcome {
    // The shared arguments are the call layer's, so they are read off here: a function
    // has no parameter of that name, and one that arrived as a keyword argument is a name
    // the caller may not spell. The same list the tool layer takes, read in the other door
    // a declared command comes in by.
    let mut own = args.clone();
    for (name, _, _) in crate::tools::laws::ALL {
        own.remove(*name);
    }
    let mut call = Args::new();
    call.insert("id", route.skill.clone());
    call.insert(
        "cmd",
        serde_json::json!({"name": route.function, "args": own.as_json()}),
    );
    run_skill(d, &call, caller).await
}

/// Arms a pattern, and answers with what it armed.
///
/// # Invariant
///
/// The session is the caller's, never the `--event` pattern's: a pattern names
/// what to watch, and the caller is who hears about it. Refusing without a caller
/// is deliberate -- a subscription with no addressee would match, fire, and tell
/// nobody, which is the one failure a registry cannot notice by itself.
async fn subscribe(d: &Dispatch, args: &Args, caller: Option<&str>) -> Outcome {
    let pattern = args.word("pattern").unwrap_or("").trim();
    if pattern.is_empty() {
        return Outcome::err(
            "subscribe needs a pattern over event names, e.g. `turn_end`".to_string(),
        );
    }
    let given = session_of(args, caller);
    let Some(session) = given.filter(|name| !name.trim().is_empty()) else {
        return Outcome::err(
            "subscribe notices a session, and neither the call nor its client named one: \
         run it from a session's shell, where `$GONGGA_SESSION` is set",
        );
    };
    let session = session.trim().to_string();
    match d.store.get_session(&session) {
        Ok(Some(_)) => {}
        Ok(None) => return Outcome::err(format!("no session named {session:?} to notice")),
        Err(e) => return Outcome::err(e.to_string()),
    }
    let action = match args.value("cmd") {
        Some(raw) => match parse_cmd(raw) {
            Ok(action) => action,
            Err(why) => return Outcome::err(why),
        },
        None => {
            return Outcome::err(format!(
                "subscribe runs a command on a match: `subscribe <pattern> <cmd>` \
             {session}"
            ));
        }
    };
    let persistent = match flag_of(args, "persistant") {
        Ok(value) => value.unwrap_or(false),
        Err(_) => {
            return Outcome::err(format!(
                "--persistant takes {}",
                protocol::SHOW_VALUES.join(" or ")
            ));
        }
    };
    let named = args
        .word("id")
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .map(str::to_string);
    let name = match named {
        Some(name) => name,
        None => match free_subscription_name(d, &session) {
            Ok(name) => name,
            Err(e) => return Outcome::err(e),
        },
    };
    match d
        .subscriptions
        .arm(&session, &name, pattern, persistent, Some(action.clone()))
        .await
    {
        Ok(row) => {
            tracing::info!(
                session = %session, subscription = row.id, name = %name, pattern = %pattern,
                cmd = %action.name, persistent, "subscribed"
            );
            Outcome::ok_with(
                format!(
                    "subscription `{name}` armed: a match of `{pattern}` runs `{}` in \
                 {session}{}",
                    action.name,
                    if persistent { ", every time" } else { " once" }
                ),
                json!({
                    "id": row.id,
                    "session": session,
                    "subscription": name,
                    "event": pattern,
                    "cmd": action.name,
                    "persistant": persistent,
                }),
            )
        }
        Err(e) => Outcome::err(e),
    }
}

/// What a match runs, from the one argument the row gives it.
///
/// # Invariant
///
/// An object is the whole call (`{"name":…,"args":{…}}`), and a bare word is a name
/// with no arguments: the same two shapes `exec` takes for a cmd's `args`.
fn parse_cmd(value: &serde_json::Value) -> Result<crate::subscriptions::Action, String> {
    if let Some(text) = value.as_str() {
        let text = text.trim();
        if text.is_empty() {
            return Err("subscribe needs a command to run on a match".to_string());
        }
        return Ok(crate::subscriptions::Action {
            name: text.to_string(),
            args: Args::new(),
        });
    }
    let name = value
        .get("name")
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .ok_or_else(|| {
            "`cmd` needs a `name`: {\"name\": \"send_message\", \"args\": {}}".to_string()
        })?;
    let table = match value.get("args") {
        None => Args::new(),
        Some(given) => Args::from_json(given.clone()).map_err(|why| format!("`cmd.args` {why}"))?,
    };
    Ok(crate::subscriptions::Action {
        name: name.to_string(),
        args: table,
    })
}

/// The first `sub<n>` this session is not using, for a call that named no subscription.
fn free_subscription_name(d: &Dispatch, session: &str) -> Result<String, String> {
    let rows = d
        .store
        .subscriptions()
        .map_err(|e| format!("could not read the armed subscriptions: {e}"))?;
    let taken: std::collections::HashSet<String> = rows
        .into_iter()
        .filter(|row| row.session == session)
        .map(|row| row.name)
        .collect();
    (1..)
        .map(|n| format!("sub{n}"))
        .find(|name| !taken.contains(name.as_str()))
        .ok_or_else(|| "no free subscription name".to_string())
}

/// Drops a subscription, or every one under a path; the caller drops its own.
async fn del_subscription(d: &Dispatch, args: &Args, caller: Option<&str>) -> Outcome {
    let path = args.word("path").unwrap_or("").trim();
    let given = session_of(args, caller);
    let Some(caller) = given.filter(|name| !name.trim().is_empty()) else {
        return Outcome::err(
            "del_subscription drops what a session armed, and neither the call nor its \
         client named one: run it from a session's shell, where `$GONGGA_SESSION` is set",
        );
    };
    let caller = caller.trim();
    // SPEC: a path takes what is under it -- `hook/` drops `hook/a` and `hook/a/b`.
    let doomed: Vec<String> = match d.store.subscriptions() {
        Ok(rows) => rows
            .into_iter()
            .filter(|row| row.session == caller && under(&row.name, path))
            .map(|row| row.name)
            .collect(),
        Err(e) => return Outcome::err(format!("could not read the subscriptions: {e}")),
    };
    if doomed.is_empty() {
        return Outcome::err(format!("no subscription named {path:?} for {caller}"));
    }
    let mut dropped = 0;
    for name in &doomed {
        match d.subscriptions.disarm_name(caller, name).await {
            Ok(n) => dropped += n,
            Err(e) => return Outcome::err(e),
        }
    }
    Outcome::ok(format!(
        "dropped {dropped} subscription(s) under {path:?}: {}",
        doomed.join(", ")
    ))
}

/// Publishes one event under a chosen name, as a log row every matcher sees.
async fn trigger_event(d: &Dispatch, args: &Args) -> Outcome {
    let event = args.word("event").unwrap_or("").trim();
    if event.is_empty() {
        return Outcome::err("trigger_event needs a name: `trigger_event <event>`");
    }
    d.agent.log_event(None, event, &json!({}));
    Outcome::ok(format!("event `{event}` published"))
}

/// The log, after a cursor: what a subscriber reads, and what a watcher polls.
///
/// # Invariant
///
/// One row in, one event out, and the payload is passed through untouched. This
/// handler knows no kinds -- it cannot, because the point of the log is that a
/// new kind is a row, not a wire change. A payload that is not JSON is kept as a
/// string rather than dropped: a row that exists is a fact, and a reader that
/// cannot parse it is better off seeing that than seeing nothing.
async fn events(d: &Dispatch, args: &Args, caller: Option<&str>) -> Outcome {
    let after = match count_of(args, "since") {
        Ok(None) => 0,
        Ok(Some(n)) if n >= 0 => n,
        Ok(Some(n)) => return Outcome::err(format!("since wants a log id, not {n}")),
        Err(e) => return Outcome::err(e),
    };
    let kind = args.word("kind").filter(|k| !k.is_empty());
    let session = match session_of(args, caller) {
        None => None,
        Some(name) => match d.sessions.get(&name) {
            Ok(s) => Some(s.id()),
            Err(e) => return Outcome::err(e.to_string()),
        },
    };
    let limit = match count_of(args, "limit") {
        Ok(None) => 200,
        Ok(Some(n)) if n > 0 => (n as u32).min(5000),
        Ok(Some(n)) => return Outcome::err(format!("limit wants a count above zero, not {n}")),
        Err(e) => return Outcome::err(e),
    };
    let rows = match d.store.events_after(after, kind, session, limit) {
        Ok(rows) => rows,
        Err(e) => return Outcome::err(e.to_string()),
    };
    // Names for the rows that have one, read once as a map: one query per row
    // would be silly.
    let names: std::collections::HashMap<i64, String> = match d.sessions.list() {
        Ok(list) => list.into_iter().map(|r| (r.id, r.name)).collect(),
        Err(_) => std::collections::HashMap::new(),
    };
    let mut events = Vec::with_capacity(rows.len());
    let mut last_id = after;
    for row in &rows {
        let payload = serde_json::from_str(&row.payload)
            .unwrap_or_else(|_| serde_json::Value::String(row.payload.clone()));
        last_id = row.id;
        events.push((
            Some(row.seq.max(0) as u64),
            Event::Record {
                id: row.id,
                seq: row.seq,
                session: row.session_id.and_then(|id| names.get(&id).cloned()),
                kind: row.kind.clone(),
                payload,
                at: row.created_at,
            },
        ));
    }
    let message = if rows.is_empty() {
        format!("no rows after id {after}")
    } else {
        format!("{} row(s), id {after}..{last_id}", rows.len())
    };
    Outcome::ok_with(
        message,
        json!({
            "count": rows.len(),
            "last_id": last_id,
            "since": after,
        }),
    )
    .with_events(events)
}

/// The reply `get_messages` makes: the window and where the transcript ends.
///
/// # Invariant
///
/// `last_seq` and `status` come from the same snapshot as the rows, so the reply
/// answers "these rows, as of this moment" without a second read.
///
/// Split out from the handler so the shape the law names can be asserted without
/// standing up an agent and a bus -- the fields are the contract here, not the I/O.
fn transcript_outcome(name: &str, t: Transcript) -> Outcome {
    let message = if t.messages.is_empty() {
        format!(
            "no rows in that range in {name} (transcript ends at seq {})",
            t.last_seq
        )
    } else {
        let first = t.messages[0].seq;
        let last = t.messages[t.messages.len() - 1].seq;
        format!(
            "{} row(s) at seq {first}..{last} in {name} (transcript ends at seq {})",
            t.messages.len(),
            t.last_seq
        )
    };
    let events = t
        .messages
        .iter()
        .filter_map(|row| {
            row_event(name, row, false).map(|event| (Some(row.seq.max(0) as u64), event))
        })
        .collect();
    // The rows are the events; a long read is many small messages, not one that
    // cannot be carried.
    Outcome::ok_with(
        message,
        json!({
            "session": name,
            "status": t.status,
            "last_seq": t.last_seq,
        }),
    )
    .with_events(events)
}

/// A stored row goes back out as the event it was published as when it arrived.
///
/// # Invariant
///
/// The shape is not this function's to invent, or a read could show a line no
/// front-end drew live. Nothing to show is nothing to send, and `REPLAY_TURN`
/// keeps a re-sent row out of the block a running turn is still writing.
fn row_event(name: &str, row: &MessageRow, drawn: bool) -> Option<Event> {
    if row.text.trim().is_empty() {
        return None;
    }
    // The harness's own words: a row the model reads as a user turn, drawn as the
    // harness rather than as the operator.
    if row.kind == "inject" {
        return Some(Event::Response(ResponseEvent {
            session: name.to_string(),
            turn: REPLAY_TURN,
            part: ResponsePart::Inject,
            text: row.text.clone(),
            done: true,
            call_id: None,
        }));
    }
    // The operator's own message: a line, exactly where the echo put it.
    if row.role.as_deref() == Some("user") {
        return Some(Event::Response(ResponseEvent {
            session: name.to_string(),
            turn: REPLAY_TURN,
            part: ResponsePart::Input,
            text: row.text.clone(),
            done: true,
            call_id: None,
        }));
    }
    // A tool's output is addressed by the call that asked for it, never by a turn.
    if row.kind == "tool_result" {
        return Some(Event::ToolAnswer(ToolAnswerEvent {
            session: name.to_string(),
            call_id: row.call_id.clone().unwrap_or_default(),
            tool: row.name.clone().unwrap_or_default(),
            stream: ToolStream::Stdout,
            text: row.text.clone(),
            done: true,
            // A row written before the verdict was kept says nothing about it.
            head: row
                .ok
                .map(|ok| crate::agent::result_head(ok, row.elapsed_ms.unwrap_or(0))),
        }));
    }
    // The call itself, as the item it was asked as: one event, never a stream.
    if row.kind == "tool_call" {
        return Some(Event::ToolCall(protocol::ToolCallEvent {
            session: name.to_string(),
            call_id: row.call_id.clone().unwrap_or_default(),
            head: crate::agent::call_head(row.name.as_deref().unwrap_or_default(), &row.text),
        }));
    }
    let part = match row.kind.as_str() {
        "reasoning" => ResponsePart::Reasoning,
        "builtin_call" => ResponsePart::Activity,
        "text" => ResponsePart::Output,
        // The harness's own rows: a reader gets a log line, a refilled window nothing.
        _ => {
            return (!drawn).then(|| Event::Log {
                level: LogLevel::Info,
                message: row.text.clone(),
                session: None,
            });
        }
    };
    Some(Event::Response(ResponseEvent {
        session: name.to_string(),
        turn: REPLAY_TURN,
        part,
        text: row.text.clone(),
        done: true,
        // A replayed call streams under the id its result named.
        call_id: row.call_id.clone(),
    }))
}

/// Where a worktree's repository is looked up: the stated directory, in the spelling
/// of the machine that owns it.
///
/// # Invariant
///
/// This machine reads an absolute path, expanded against this process's directory when
/// it was relative; a peer's directory is passed as written, because only that machine
/// can expand it, and nothing stated is its own default.
fn started_at(stated: Option<&str>, local: bool) -> std::path::PathBuf {
    if !local {
        return std::path::PathBuf::from(stated.unwrap_or_default().trim());
    }
    match stated {
        Some(stated) => {
            crate::session::stated_directory(stated, &std::env::current_dir().unwrap_or_default())
        }
        None => std::env::current_dir().unwrap_or_default(),
    }
}

/// Reads what a session's worktree holds and writes it down, when it owns one.
///
/// # Invariant
///
/// One git call pair at the end of a turn, and nothing on the turn's own path: the
/// numbers a card shows are a reading of a quiet worktree, and a reading that fails
/// leaves the last one standing rather than claiming either answer.
async fn read_worktree_state(d: &Dispatch, session: &crate::session::SessionHandle) {
    let name = session.name().to_string();
    let Ok(Some(row)) = d.store.get_session(&name) else {
        return;
    };
    let Some(worktree) = row.worktree.clone() else {
        return;
    };
    let space = session.space();
    match crate::worktree::state(&space, &worktree, std::path::Path::new(&row.cwd)).await {
        Ok(state) => {
            if let Err(e) = d
                .store
                .set_worktree_state(row.id, state.merged, state.changed)
            {
                tracing::warn!(session = %name, error = %e, "could not record the worktree state");
            }
        }
        Err(e) => tracing::warn!(session = %name, worktree = %worktree, error = %e,
            "could not read the worktree state"),
    }
}

/// Makes a session's worktree, wherever that session's paths live.
async fn make_worktree(
    space: &crate::space::Space,
    start: &std::path::Path,
    name: &str,
) -> Result<std::path::PathBuf, String> {
    crate::worktree::create(space, start, name).await
}

/// Takes a session's worktree away the same way, and answers what went with it.
async fn remove_worktree(
    space: &crate::space::Space,
    name: &str,
    cwd: &std::path::Path,
) -> Result<crate::worktree::Removed, String> {
    crate::worktree::remove(space, name, cwd).await
}

/// Takes away the worktree a session owns, if it owns one.
///
/// # Invariant
///
/// Run before anything of the session is stopped, so a worktree git will not let go
/// leaves the session exactly as it was: interrupted and then not deleted is worse
/// than not deleted, and a branch left behind is one no delete will ever own.
async fn remove_worktree_of(
    d: &Dispatch,
    name: &str,
) -> Result<Option<(String, crate::worktree::Removed)>, String> {
    let Some(row) = d.store.get_session(name).ok().flatten() else {
        return Ok(None);
    };
    let Some(worktree) = row.worktree else {
        return Ok(None);
    };
    let space = d.sessions.space(name).map_err(|e| e.to_string())?;
    let removed = remove_worktree(&space, &worktree, std::path::Path::new(&row.cwd)).await?;
    Ok(Some((worktree, removed)))
}

fn prepare_delete(d: &Dispatch, name: &str) -> (bool, usize) {
    // A delete drops the turn even when it was waiting: nothing of it is kept.
    let interrupted = interrupt(d, name, Stop::Delete);
    if let Some(session) = d.sessions.peek(name) {
        session.mark_deleted();
    }
    interrupted
}

async fn finish_delete(
    d: &Dispatch,
    name: &str,
    was_busy: bool,
    dropped: usize,
    taken: Option<(String, crate::worktree::Removed)>,
) -> Outcome {
    let session = d.sessions.peek(name);
    let _gate = match &session {
        Some(session) => Some(session.claim().await),
        None => None,
    };
    // Its channels are programs it started: each is told to end, and its row goes.
    let channels = d.agent.channels.stop_session(name).await;
    if channels > 0 {
        tracing::info!(session = %name, channels, "channels ended with their session");
    }
    // A delete waits for no process: what it cannot wait for is ended by one task.
    let bash = d.sessions.detached().detach_session(name).await;
    let stopping_bash = bash.len();
    let shells = session.clone();
    if stopping_bash > 0 || shells.is_some() {
        tokio::spawn(async move {
            for mut job in bash {
                if !job.stop().await {
                    tracing::warn!(pid = job.pid, name = %job.name,
                    "a deleted session's background bash did not stop");
                }
            }
            if let Some(shells) = shells {
                shells.closing();
            }
        });
    }
    match d.sessions.delete(name) {
        Ok(true) => {
            // SPEC: a deleted session's bash go with it, and so do its
            // subscriptions -- there is nobody left to tell.
            let forgotten = d.subscriptions.forget_session(name).await;
            publish_sessions(d);
            let mut message = format!("deleted session {name}");
            if stopping_bash > 0 {
                message.push_str(&format!("; stopping {stopping_bash} background bash(es)"));
            }
            if was_busy {
                message.push_str(" (interrupted the running turn)");
            }
            if dropped > 0 {
                message.push_str(&format!("; dropped {dropped} pending task(s)"));
            }
            if forgotten > 0 {
                message.push_str(&format!("; dropped {forgotten} subscription(s)"));
            }
            if let Some((worktree, gone)) = &taken {
                match (&gone.path, gone.branch) {
                    (Some(path), true) => message.push_str(&format!(
                        "; its worktree at {} and the branch {worktree:?} went too",
                        path.display()
                    )),
                    (Some(path), false) => message.push_str(&format!(
                        "; its worktree at {} went too, and the branch {worktree:?} was                          already gone",
                        path.display()
                    )),
                    (None, _) => {
                        message.push_str(&format!("; its worktree {worktree:?} was already gone"))
                    }
                }
            }
            Outcome::ok(message)
        }
        Ok(false) => Outcome::err(format!("no session named {name:?}")),
        Err(e) => Outcome::err(e.to_string()),
    }
}

async fn del_session(d: &Dispatch, args: &Args) -> Outcome {
    let path = args.word("path").unwrap_or_default().to_string();
    let rows = match d.store.list_sessions() {
        Ok(rows) => rows,
        Err(e) => return Outcome::err(format!("could not read the sessions: {e}")),
    };
    let mut doomed: Vec<String> = rows
        .iter()
        .filter(|row| under(&row.name, &path))
        .map(|row| row.name.clone())
        .collect();
    // SPEC's Dreaming: a source's delete discards its dreams, even ones a rename
    // left outside the path.
    let doomed_ids: std::collections::HashSet<i64> = rows
        .iter()
        .filter(|row| under(&row.name, &path))
        .map(|row| row.id)
        .collect();
    let mut seen: std::collections::HashSet<String> = doomed.iter().cloned().collect();
    for row in &rows {
        if row
            .dream_of
            .is_some_and(|source| doomed_ids.contains(&source))
            && seen.insert(row.name.clone())
        {
            doomed.push(row.name.clone());
        }
    }
    let Some((one, below)) = doomed.split_first() else {
        return Outcome::err(format!("no session named {path:?}"));
    };
    if below.is_empty() {
        // SPEC's `Workspace.worktree` goes first: a git that will not take it keeps the
        // session whole.
        let taken = match remove_worktree_of(d, one).await {
            Ok(taken) => taken,
            Err(e) => return Outcome::err(format!("session {one} was not deleted: {e}")),
        };
        let (was_busy, dropped) = prepare_delete(d, one);
        return finish_delete(d, one, was_busy, dropped, taken).await;
    }
    // A path takes what is under it; each delete keeps its own cleanup, so one by one.
    let mut deleted = 0;
    let mut refused = Vec::new();
    for name in &doomed {
        let taken = match remove_worktree_of(d, name).await {
            Ok(taken) => taken,
            Err(e) => {
                refused.push(format!("{name}: {e}"));
                continue;
            }
        };
        let (was_busy, dropped) = prepare_delete(d, name);
        match finish_delete(d, name, was_busy, dropped, taken).await {
            outcome if outcome.ok => deleted += 1,
            outcome => refused.push(format!("{name}: {}", outcome.message)),
        }
    }
    let mut message = format!(
        "deleted {deleted} session(s) under {path:?}: {}",
        doomed.join(", ")
    );
    if !refused.is_empty() {
        message.push_str(&format!(
            "; {} session(s) were left in place: {}",
            refused.len(),
            refused.join("; ")
        ));
    }
    Outcome::ok(message)
}

/// Which verb stopped a turn, and what it leaves behind.
///
/// See `docs/notes/server-commands.md`.
///
/// # Invariant
///
/// SPEC splits the three: `cancel` is the only one that writes a flag a restart reads,
/// `interrupt_session` ends the turn where it stands, and a delete owes nothing at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Stop {
    /// `cancel`: queue the notices, and let a restart continue this turn.
    Cancel,
    /// `interrupt_session`: end the wait and the loop; the turn is owed nothing, the inbox stays.
    Interrupt,
    /// `del_session`: the rows are going away.
    Delete,
}

/// Stops one session's turn.
///
/// # Invariant
///
/// SPEC gives the canceled flag to `cancel` alone, and `cancel` writes it itself; this
/// fires the token and records which verb fired it, so the turn's own rows and the
/// session's status can say whether a restart continues it.
///
/// The inbox survives every stop but a delete: a queued message is received input,
/// and everything received must be saved to context.
fn interrupt(d: &Dispatch, name: &str, why: Stop) -> (bool, usize) {
    let Some(session) = d.sessions.peek(name) else {
        return (false, 0);
    };
    // Read before the cancel: afterwards there is nothing left to ask.
    let was_busy = session.busy();
    // Only a `cancel` keeps its rows for the next start: that flag is cancel's alone.
    let keeps = why != Stop::Delete;
    if keeps && was_busy {
        match why {
            Stop::Cancel => session.mark_resumable(),
            Stop::Interrupt => session.mark_interrupted(),
            Stop::Delete => {}
        }
    }
    session.cancel();
    // The inbox survives every stop but a delete; only a turn's owed row is ever dropped here.
    let dropped = match why {
        Stop::Delete => d.store.clear_pending_tasks(name).unwrap_or(0),
        _ if was_busy => 0,
        _ => {
            // An idle interrupt has no turn to fold the inbox in; save it to context here.
            if why == Stop::Interrupt {
                d.agent.deliver_inbox(session.id(), name);
            }
            d.store
                .clear_pending_tasks_except(name, "inject")
                .unwrap_or(0)
        }
    };
    (was_busy, dropped)
}

async fn interrupt_session(d: &Dispatch, args: &Args) -> Outcome {
    let name = args.word("id").unwrap_or_default().to_string();
    // `peek`, not `get`: materialising would start shells just to stop nothing.
    if d.sessions.peek(&name).is_none() {
        return match d.store.get_session(&name) {
            Ok(Some(_)) => Outcome::ok(format!("{name} is not running; nothing to interrupt")),
            Ok(None) => Outcome::err(format!("no session named {name:?}")),
            Err(e) => Outcome::err(e.to_string()),
        };
    }
    let (was_busy, dropped) = interrupt(d, &name, Stop::Interrupt);
    let mut message = if was_busy {
        format!("interrupted {name}; what it received is in its context")
    } else {
        format!("{name} is idle")
    };
    if dropped > 0 {
        message.push_str(&format!("; dropped {dropped} pending task(s)"));
    }
    Outcome::ok(message)
}

/// A stop from outside the protocol: a signal, a dead window, Ctrl-C.
///
/// # Invariant
///
/// It runs the same `cancel`. Only a protocol `cancel` lets jobs go, so this
/// one ends them all. See `docs/notes/server-signal.md`.
pub(crate) async fn stop(d: &Dispatch) {
    // A window that cancelled as it left has already asked; this is the echo.
    if d.shutting_down.load(Ordering::SeqCst) {
        return;
    }
    let reason = crate::signal::reason()
        .unwrap_or("a signal reached the server")
        .to_string();
    let said = cancel(d, &Origin::Signal { reason }, false).await;
    tracing::info!(answer = %said.message, "stopped as a signal asks");
}

/// Interrupts every turn, records who asked, and leaves at once.
///
/// # Invariant
///
/// The record comes before the answer: it is how a window that did *not* ask
/// learns the server is stopping, and its `payload.by` is the only place that
/// says who asked. The status that follows from it states the fact, not the
/// name -- see `docs/notes/server-commands.md`.
async fn cancel(d: &Dispatch, origin: &Origin, keep_alive: bool) -> Outcome {
    // The listen loop keeps dispatching through the notice window.
    d.shutting_down.store(true, Ordering::SeqCst);
    // SPEC: every *running* session; an idle one has nothing to interrupt.
    let running: Vec<_> = d
        .sessions
        .live_sessions()
        .into_iter()
        .filter(|session| session.busy())
        .collect();
    // SPEC's two flags, written per session before anything is interrupted.
    let mut waiting = 0;
    for session in &running {
        let until = session.stop_signal().wait_until();
        waiting += usize::from(until.is_some());
        if let Err(e) = d.store.record_stop(session.id(), until.is_none(), until) {
            tracing::warn!(
                session = %session.name(), error = %e,
                "could not record what this stop leaves the next start"
            );
        }
    }
    for session in &running {
        // Fire the token: a stuck call has no boundary. A waiting one gets a deadline.
        interrupt(d, session.name(), Stop::Cancel);
    }
    let pending = d.store.pending_tasks().map(|t| t.len()).unwrap_or(0);
    // SPEC: `keep_alive` detaches every channel -- a job, not a row (`tools::job::arm`).
    let channels = match keep_alive {
        true => {
            // The row carries the flag, so a later stop that asked for nothing spares
            // these too, and the next start takes them over.
            let marked = d.agent.channels.mark_kept().await;
            if marked > 0 {
                tracing::info!(marked, "cancel: every channel was marked to be kept");
            }
            let released = crate::tools::detached::release_detached();
            if released > 0 {
                tracing::info!(released, "cancel: every background tree was let go");
            }
            None
        }
        false => Some(d.agent.channels.stop_every().await),
    };
    let by = origin.describe();
    d.agent.log_event(
        None,
        protocol::SERVER_STOP,
        &json!({
            "by": by,
            "sessions": running.len(),
            "pending": pending,
            "channels": channels.unwrap_or(0),
            "kept_channels": keep_alive,
        }),
    );
    // Wait before shutting down: the acknowledgement is published after this returns.
    let shutdown = d.shutdown.clone();
    tokio::spawn(async move {
        tokio::time::sleep(SHUTDOWN_NOTICE).await;
        shutdown.cancel();
    });
    let channels = match channels {
        Some(ended) => format!("; {ended} channel(s) ended with this server"),
        None => "; every channel was kept alive, and the next start adopts them".to_string(),
    };
    let continuation = match waiting {
        0 => "their turns are kept and the next start continues them".to_string(),
        waiting if waiting == running.len() => {
            "every one was inside `wait`: the next start continues those waits for the time \
         that was left"
                .to_string()
        }
        waiting => format!(
            "{waiting} of them were inside `wait` and the next start continues those waits \
         for the time that was left; the rest continue at once"
        ),
    };
    let said = format!(
        "interrupted {} session(s); {continuation}{channels}",
        running.len()
    );
    tracing::warn!(
        running = running.len(),
        by = %by,
        "the server is stopping: every turn was interrupted"
    );
    Outcome::ok(said)
}

fn recovery_groups(
    tasks: Vec<PendingTask>,
) -> std::collections::BTreeMap<String, Vec<PendingTask>> {
    let mut groups = std::collections::BTreeMap::<String, Vec<PendingTask>>::new();
    for task in tasks {
        if matches!(task.kind.as_str(), "turn" | "inject") {
            groups.entry(task.session.clone()).or_default().push(task);
        }
    }
    groups
}

/// Whether the rows this session holds are news it owes a *turn* for.
///
/// # Invariant
///
/// News a session is owed -- a message, or a notice about work something was waiting
/// for -- owes a turn, so an idle session is woken for it. That is what a `wait` survives
/// a stop by: SPEC leaves the news it waited for to the next start, and the next start
/// delivers it to a session that is doing nothing.
fn owes_a_turn(tasks: &[PendingTask]) -> bool {
    tasks
        .iter()
        .any(|task| task.kind == "turn" || (task.kind == "inject" && !reads_at_its_own_turn(task)))
}

/// A row the session reads at its next turn, and never a reason to start one.
///
/// # Invariant
///
/// Two rows are the server talking about news no turn is owed for: `explore` is advice
/// about the turn already running, and SPEC's `cancel` queues what it leaves behind -- a
/// channel that ended while nothing waited on it. An idle session woken by either would
/// be the turn nobody asked for.
fn reads_at_its_own_turn(task: &PendingTask) -> bool {
    task.payload
        .get("queued")
        .and_then(|queued| queued.as_bool())
        == Some(true)
        || task.payload.get("notice").and_then(|kind| kind.as_str()) == Some("explore")
}

/// Wakes each session with stranded work at most once.
pub fn nudge_inboxes(d: &Dispatch) -> usize {
    if d.shutting_down.load(Ordering::SeqCst) || d.shutdown.is_cancelled() {
        return 0;
    }
    let Ok(tasks) = d.store.pending_tasks() else {
        return 0;
    };
    // A stop left this session inside `wait`; its clock decides when it starts.
    let waiting: std::collections::HashMap<String, (i64, i64)> = d
        .store
        .waiting_sessions()
        .unwrap_or_default()
        .into_iter()
        .map(|(id, name, until)| (name, (id, until)))
        .collect();
    let now = crate::store::now_millis();
    let mut started = 0;
    for (name, tasks) in recovery_groups(tasks) {
        if let Some((id, until)) = waiting.get(&name)
            && *until > now
        {
            // A `wait` ends on what arrives; these rows are that, so the clock is done.
            if !owes_a_turn(&tasks) {
                continue;
            }
            if let Err(e) = d.store.record_stop(*id, false, None) {
                tracing::warn!(session = %name, error = %e, "could not clear a wait its news ended");
            }
        }
        let Ok(session) = d.sessions.get(&name) else {
            continue;
        };
        let owned = tasks
            .iter()
            .filter(|task| task.kind == "turn")
            .map(|task| task.id)
            .collect();
        let owed = owes_a_turn(&tasks);
        if let Some(work) = claim_recovery(d, session, owned, owed) {
            spawn_turn(d, work);
            started += 1;
        }
    }
    started
}

/// Replays interrupted work with one recovery owner per session.
///
/// # Invariant
///
/// Every draft becomes a row marked `interrupted`, so half a message is half a
/// message rather than nothing at all. Run before anything serves, because the
/// next request is built from these rows. See `docs/notes/server-store.md`.
pub fn settle_drafts(d: &Dispatch) -> usize {
    let Ok(sessions) = d.store.list_sessions() else {
        return 0;
    };
    let mut settled = 0;
    for row in sessions {
        match d.store.settle_drafts(row.id) {
            Ok(0) => {}
            Ok(count) => {
                tracing::info!(
                    session = %row.name, count,
                    "kept what a stream had said when the last run stopped"
                );
                settled += count;
            }
            Err(e) => tracing::warn!(
                session = %row.name, error = %e,
                "could not keep the half-answers the last run left behind"
            ),
        }
    }
    settled
}

/// Answers every call the last run left open, in every session.
///
/// # Invariant
///
/// Run before anything may build a request, and for every session: a transcript
/// with an unanswered call fails every turn until somebody looks at it. The
/// answer is appended, not invented while writing the body, so it is the same
/// answer for every later request. See `docs/notes/server-store.md`.
pub fn answer_open_calls(d: &Dispatch) -> usize {
    let Ok(sessions) = d.store.list_sessions() else {
        return 0;
    };
    let mut answered = 0;
    for row in sessions {
        match d.store.answer_open_calls(row.id) {
            Ok(0) => {}
            Ok(count) => {
                tracing::info!(
                    session = %row.name, count,
                    "answered calls a run that died inside a tool left open"
                );
                answered += count;
            }
            Err(e) => tracing::warn!(
                session = %row.name, error = %e,
                "could not answer the calls a dead run left open"
            ),
        }
    }
    answered
}

/// Discards dreams whose true-end discard was lost to the last process exit.
pub async fn sweep_orphan_dreams(d: &Dispatch) -> usize {
    let Ok(orphans) = d.store.orphan_dreams() else {
        return 0;
    };
    let mut swept = 0;
    for (_, name, source, generation) in orphans {
        let (was_busy, dropped) = prepare_delete(d, &name);
        let said = finish_delete(d, &name, was_busy, dropped, None).await;
        if !said.ok {
            tracing::warn!(session = %name, answer = %said.message, "an orphan dream was not discarded");
            continue;
        }
        swept += 1;
        // The generation may dream again, but only if the source still marks it.
        if let Some(generation) = generation {
            if let Err(e) = d.store.reset_dreamed_checkpoint(source, generation) {
                tracing::warn!(session = %name, error = %e, "could not reset the dreamed generation");
            }
        }
        d.agent.log_event(
            None,
            "dream_sweep",
            &json!({"dream": name, "source": source}),
        );
        tracing::info!(session = %name, "orphan dream discarded: its end was lost to a process exit");
    }
    swept
}

pub async fn resume_pending(d: &Dispatch) -> usize {
    let Ok(tasks) = d.store.pending_tasks() else {
        return 0;
    };
    let known: std::collections::HashSet<i64> = tasks
        .iter()
        .filter(|task| matches!(task.kind.as_str(), "turn" | "inject"))
        .map(|task| task.id)
        .collect();
    for task in tasks.iter().filter(|task| !known.contains(&task.id)) {
        let _ = d.store.delete_pending_task(task.id);
    }
    let mut resumed = 0;
    // The sessions this pass already gave a turn, so the flag below adds nothing.
    let mut woken: std::collections::HashSet<String> = std::collections::HashSet::new();
    // A wait still running is a reason not to start this session's rows now.
    let waiting: std::collections::HashMap<String, (i64, i64)> = d
        .store
        .waiting_sessions()
        .unwrap_or_default()
        .into_iter()
        .map(|(id, name, until)| (name, (id, until)))
        .collect();
    let now = crate::store::now_millis();
    for (name, tasks) in recovery_groups(tasks) {
        let Ok(session) = d.sessions.get(&name) else {
            // Only a confirmed missing session may discard owed work.
            if matches!(d.store.get_session(&name), Ok(None)) {
                for task in tasks {
                    let _ = d.store.delete_pending_task(task.id);
                }
            }
            continue;
        };
        // Every row still here is news this build wrote: none is dropped.
        let kept = tasks;
        let owed = owes_a_turn(&kept);
        // Rows already here are the news the wait waited for; with none, the clock decides.
        if let Some((id, until)) = waiting.get(&name)
            && *until > now
        {
            if !owed {
                continue;
            }
            if let Err(e) = d.store.record_stop(*id, false, None) {
                tracing::warn!(session = %name, error = %e, "could not clear a wait its news ended");
            }
        }
        let owned = kept
            .iter()
            .filter(|task| task.kind == "turn")
            .map(|task| task.id)
            .collect();
        if let Some(work) = claim_recovery(d, session, owned, owed) {
            tracing::info!(session = %name, "replaying work left pending");
            spawn_turn(d, work);
            woken.insert(name);
            resumed += 1;
        }
    }
    // SPEC: a restart continues the wait for what was left -- a clock, not a replay.
    for (id, name, until) in d.store.waiting_sessions().unwrap_or_default() {
        let Ok(session) = d.sessions.get(&name) else {
            continue;
        };
        if session.busy() {
            continue;
        }
        let left = until - now;
        let later = Arc::new(d.clone());
        let owed = name.clone();
        tokio::spawn(async move {
            if left > 0 {
                tokio::time::sleep(Duration::from_millis(u64::try_from(left).unwrap_or(0))).await;
            }
            // News may have answered this wait: only the row still being there says
            // this clock is the one that ends it.
            let still = later
                .store
                .waiting_sessions()
                .unwrap_or_default()
                .into_iter()
                .any(|(_, name, _)| name == owed);
            if !still {
                return;
            }
            // Cleared here, not when read: until it fires the row holds a nudge back.
            if let Err(e) = later.store.record_stop(id, false, None) {
                tracing::warn!(session = %owed, error = %e, "could not clear a resumed wait");
            }
            // A stop that arrives during the wait is the newer word: nothing starts.
            if later.shutting_down.load(Ordering::SeqCst) || later.shutdown.is_cancelled() {
                return;
            }
            // A turn started while this one waited is what the wait was for.
            if session.busy() {
                return;
            }
            let rows = later.store.pending_tasks().unwrap_or_default();
            let owned: Vec<i64> = rows
                .iter()
                .filter(|task| task.session == owed && task.kind == "turn")
                .map(|task| task.id)
                .collect();
            if let Some(work) = claim_recovery(&later, session, owned, true) {
                tracing::info!(
                    session = %owed,
                    waited_ms = left.max(0),
                    "continuing the wait a stop cut short"
                );
                spawn_turn(&later, work);
            }
        });
        resumed += 1;
    }
    // SPEC: "restart will consume and continue session with cancel flag".
    for (id, name) in d.store.canceled_sessions().unwrap_or_default() {
        if woken.contains(&name) {
            continue;
        }
        let Ok(session) = d.sessions.get(&name) else {
            continue;
        };
        if session.busy() {
            continue;
        }
        if let Some(work) = claim_recovery(d, session, Vec::new(), true) {
            tracing::info!(
                session = %name, id,
                "continuing the turn a stop interrupted"
            );
            spawn_turn(d, work);
            resumed += 1;
        }
    }
    resumed
}

/// Fills the published picture from the database, announcing nothing.
///
/// # Invariant
///
/// The picture a greeting hands every client is the one [`publish_sessions`]
/// compares against. A server that started with sessions already in its database
/// would otherwise diff against nothing: it would announce every row as new, and
/// a deletion -- the one change a client cannot work out for itself -- would be
/// the one thing it never heard.
pub fn prime_sessions(d: &Dispatch) {
    let Ok(rows) = d.sessions.list() else {
        return;
    };
    let picture: Vec<protocol::SessionInfo> = rows.iter().map(|r| session_info(d, r)).collect();
    *d.published.lock().unwrap_or_else(|e| e.into_inner()) = picture;
}

/// Publishes what changed in the session list, one row at a time.
///
/// # Invariant
///
/// A delta, never the table: a change costs one row, and the names that left are
/// published too, because a client cannot tell "gone" from "unchanged". A rename is
/// therefore two rows: the old name gone, the new one stated.
pub fn publish_sessions(d: &Dispatch) {
    let Ok(rows) = d.sessions.list() else {
        return;
    };
    let now: Vec<protocol::SessionInfo> = rows.iter().map(|r| session_info(d, r)).collect();
    let mut last = d.published.lock().unwrap_or_else(|e| e.into_inner());
    for old in last.iter() {
        if !now.iter().any(|new| new.name == old.name) {
            d.bus.publish(Event::SessionGone {
                name: old.name.clone(),
            });
        }
    }
    for row in &now {
        if !last.iter().any(|old| old == row) {
            d.bus.publish(Event::Session {
                session: row.clone(),
            });
        }
    }
    *last = now;
}

/// What a session would send now: its live handle when it has one, else its row.
///
/// # Invariant
///
/// A live handle was materialised from the row, so the two agree. A session nobody
/// has touched since this process started has no handle, and then the row is the
/// only record of what it was told: reading `Tuning::default()` there reports a
/// session that runs on no model at all.
fn stated_tuning(
    live: Option<crate::session::Tuning>,
    row: &crate::store::SessionRow,
) -> crate::session::Tuning {
    match live {
        Some(tuning) => tuning,
        None => crate::session::Tuning {
            model: (!row.model.is_empty()).then(|| row.model.clone()),
            context_limit: row.context_limit,
            thinking_effort: row.thinking_effort.clone(),
            cache_tier: row.cache_tier.clone(),
            cache_keepalive: row.cache_keepalive,
            live_compact_enabled: row.live_compact_enabled,
            live_compact_interval_tokens: row.live_compact_interval_tokens,
        },
    }
}

/// A stored row as a client is told about it, whatever told it.
///
/// # Invariant
///
/// Every field is what the session *would do now*: its live handle when it has one,
/// else its row. One function, because the greeting and the list must not disagree.
/// `cwd` is where the shell says it is, because that is where a tool reads.
pub fn session_info(d: &Dispatch, row: &crate::store::SessionRow) -> protocol::SessionInfo {
    let live = d.sessions.peek(&row.name);
    let tuning = stated_tuning(live.as_ref().map(|s| s.tuning()), row);
    // Its own shell's directory, which is what a tool reads: the row is all
    // there is before a shell exists.
    let reported = live.map(|s| s.cwd()).unwrap_or_default();
    let cwd = if reported.as_os_str().is_empty() {
        row.cwd.clone()
    } else {
        reported.to_string_lossy().into_owned()
    };
    let entry = tuning
        .model
        .as_deref()
        .and_then(|model| d.providers.place(model));
    // The roster's spelling, else the name the session itself gave. Never the
    // server's own: no session runs on that.
    let model = entry
        .as_ref()
        .map(|e| e.model.clone())
        .or_else(|| tuning.model.clone());
    let thinking_effort = tuning.thinking_effort.clone();
    protocol::SessionInfo {
        name: row.name.clone(),
        status: row.status,
        created_at: row.created_at,
        updated_at: row.updated_at,
        ended_at: row.ended_at,
        messages: row.messages,
        // Read from the transcript, not from a count on the row: a derived number
        // would be a second answer to the same question.
        transcript: d.store.last_seq(row.id).unwrap_or(0),
        cwd,
        usage: row.usage,
        model,
        thinking_effort,
        remote: row.remote.clone(),
        worktree: row
            .worktree
            .clone()
            .zip(row.worktree_merged)
            .map(|(name, merged)| protocol::Worktree {
                name,
                merged,
                changed: row.worktree_changed.unwrap_or(true),
            }),
        context_limit: tuning
            .context_limit
            .or(Some(d.agent.config.now().agent.context_limit_percent)),
    }
}
