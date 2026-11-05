//! The command surface: catalog and dispatch.
//! See `docs/notes/server-commands.md`.

use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Duration;

use protocol::{
    Args, CommandSpec, CommandTarget, CompletionSource, Event, LogLevel, REPLAY_TURN,
    ResponseEvent, ResponsePart, ToolAnswerEvent, ToolStream,
};
use serde_json::json;

use tokio_util::sync::CancellationToken;

use crate::agent::Agent;
use crate::bus::EventBus;
use crate::session::SessionManager;
use crate::store::{MessageRow, PendingTask, Store, Transcript};

/// How long `cancel` waits before pulling the plug, so the acknowledgement
/// still reaches whoever asked for it.
const SHUTDOWN_NOTICE: Duration = Duration::from_millis(250);

/// The heartbeat's whole content: a clock reading, so a warm prefix is all it buys.
fn time_notice() -> String {
    format!("(time notice: {})", crate::clock::now())
}

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
    /// The machine's LLM roster: what `set_session --model` is checked against.
    pub providers: crate::providers::Handle,
    /// Turns currently running. `cancel` waits for this to drain before the
    /// process is allowed to exit.
    pub active: Arc<AtomicUsize>,
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

/// Owns one slot in [`Dispatch::active`], releasing it even on a panic.
///
/// # Invariant
///
/// A slot is what a stop waits for, so everything that must finish before the
/// process leaves holds one: a turn, and a subscription's action.
pub(crate) struct ActiveGuard(Arc<AtomicUsize>);

impl ActiveGuard {
    pub(crate) fn new(active: Arc<AtomicUsize>) -> Self {
        active.fetch_add(1, Ordering::SeqCst);
        Self(active)
    }
}

impl Drop for ActiveGuard {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
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
    /// A client asked: its label, its peer, and the session it ran from, if any.
    Client {
        label: String,
        peer: SocketAddr,
        session: Option<String>,
    },
    /// No client asked: a signal reached this process, or the process it follows ended.
    Signal { reason: String },
}

impl Origin {
    /// The session the command was run from, when a shell ran it.
    pub fn session(&self) -> Option<&str> {
        match self {
            Origin::Client { session, .. } => session.as_deref(),
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
            } => match session.as_deref().map(str::trim) {
                Some(session) if !session.is_empty() => {
                    format!("{label} at {peer}, from session {session}")
                }
                _ => format!("{label} at {peer}"),
            },
            Origin::Signal { reason } => reason.clone(),
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

/// Hands one durable turn to the agent, counted in [`Dispatch::active`] before
/// the task exists.
///
/// # Invariant
///
/// The slot is taken by the caller's thread, not inside the spawned task: the
/// drain after `cancel` reads the count, and a task queued but not yet polled
/// would leave it reading zero and let the process exit mid-turn.
fn spawn_turn(d: &Dispatch, work: TurnWork) {
    if d.shutting_down.load(Ordering::SeqCst) {
        // The turn's pending row stays in the database, so the next start owes it.
        return;
    }
    let agent = d.agent.clone();
    let sessions = d.sessions.clone();
    let bus = d.bus.clone();
    let active: ActiveGuard = ActiveGuard::new(d.active.clone());
    // Read before the turn starts: its first step consumes the mark.
    let keepalive_agent = d.agent.clone();
    let keepalive_d = Arc::new(d.clone());
    let keepalive_session = work.session.clone();
    let keepalive_model = d.agent.model_config.model();
    let keepalive_tuning = work.session.tuning();
    tokio::spawn(async move {
        let _active = active;
        let session = work.session.clone();
        agent
            .run_turn(&work.session, work.tasks, work.epoch, work.gate)
            .await;
        // The list is ordered by when a turn ended: publish the row that moved.
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
                    }),
                    Some(Err(e)) => bus.publish(Event::Log {
                        level: LogLevel::Error,
                        message: format!("{}: the queued move failed: {e}", session.name()),
                    }),
                    None => {}
                }
                drop(gate);
            }
        }
        // Each turn schedules the next notice, so the chain has no last one.
        if !keepalive_tuning.cache_keepalive {
            return;
        }
        let wait_ms = keepalive_agent
            .cache_fire_after_ms(&keepalive_model, keepalive_tuning.cache_tier.as_deref());
        if wait_ms == 0 {
            return;
        }
        let ended_at = crate::store::now_millis();
        let session_id = keepalive_session.id();
        let name = keepalive_session.name().to_string();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(wait_ms)).await;
            // Any request during the wait already refreshed the prefix.
            let refreshed = keepalive_d
                .store
                .last_usage_at(session_id)
                .ok()
                .flatten()
                .is_some_and(|at| at > ended_at);
            if refreshed || keepalive_session.busy() {
                return;
            }
            let _ = deliver(keepalive_d.as_ref(), &name, time_notice()).await;
        });
    });
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
    pub events: Vec<Event>,
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
    pub fn with_events(mut self, events: Vec<Event>) -> Self {
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
                // SPEC writes `--message <message> --name <session>(sticky)`:
                // the payload leads; declaration order is bare-word order.
                kind_arg(
                    "name",
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
            name: "set_session".into(),
            description:
                "Create the session if it does not exist, and apply the given settings to \
                      it. Settings omitted now are left as they are."
                    .into(),
            args: vec![
                // SPEC: `--name <session>(sticky)`. A sticky entry keys on the
                // kind `session`, wherever the argument appears.
                sticky_arg(
                    kind_arg(
                        "name",
                        "Session to create or reconfigure.",
                        true,
                        "session",
                        Some("name"),
                        CompletionSource::Sessions,
                    ),
                    true,
                ),
                // SPEC: `[--model <model>](initial: none)` -- omitted keeps
                // the session's model; a new one takes the configured default.
                kind_arg(
                    "model",
                    "The model to run this session's turns on, as <provider>/<model \
                     name>. Omit to keep the session's model, or to take the \
                     configured default on a new one.",
                    false,
                    "model",
                    Some("model"),
                    // The greeting's model table, which is the same list with the
                    // levels each model takes.
                    CompletionSource::Models,
                ),
                kind_arg(
                    "thinking",
                    "How hard the model should think, in the roster's spelling (`low`, \
                     `high`, `max`, ...): what completes is exactly what the model this \
                     session runs on takes. `default` names the level a model that is \
                     told nothing uses, clearing one set earlier.",
                    false,
                    "thinking",
                    Some("level"),
                    // Nothing to offer through a source: the values are what the
                    // roster lets through, published as `accepts` below.
                    CompletionSource::None,
                )
                .accepting_when(crate::providers::level_accepts)
                .following("model"),
                kind_arg(
                    "ctx_limit",
                    "The context level at which this session gets a `Context notice`, \
                     e.g. `30%`. 0 never notices. Initial: a 70% share of the \
                     model's context window, or the server's token budget when the \
                     window is unknown.",
                    false,
                    "ctx_limit",
                    Some("n%"),
                    // Any 0-100 is accepted; the list is the points worth
                    // offering, not the ones allowed.
                    CompletionSource::Static(
                        [
                            "0", "10%", "20%", "30%", "40%", "50%", "60%", "70%", "80%", "90%",
                        ]
                        .iter()
                        .map(|s| s.to_string())
                        .collect(),
                    ),
                )
                .following("model"),
                kind_arg(
                    "cache_tier",
                    "The cache tier every request of this session sends, in the model's \
                     roster spelling. Omit to keep what the session has; name `none` to \
                     send no tier at all, which is the only way to say so on a model that \
                     declares none. A session that never named one sends the shortest tier \
                     its model declares.",
                    false,
                    "cache_tier",
                    Some("name"),
                    CompletionSource::None,
                )
                .accepting_when(crate::providers::tier_accepts)
                .following("model"),
                kind_arg(
                    "cache_keepalive",
                    "Whether a periodic `(time notice: ...)` message keeps this \
                     session's prefix warm after every turn. `false` by default; \
                     `true` keeps sending one per tier window with no last one.",
                    false,
                    "cache_keepalive",
                    Some("true|false"),
                    CompletionSource::Static(vec!["true".into(), "false".into()]),
                ),
                // SPEC: `[--remote <remote>](initial: none)`.
                kind_arg(
                    "remote",
                    "Which machine this session's shells run on, named by a `[remotes.<name>]` \
                     table in this machine's `~/.agents/remotes.toml`. Omit for this \
                     machine, and name `none` to move a session back here. Naming a \
                     different one moves an existing session: every shell on the old \
                     machine is ended first, and a move asked for during a turn waits for \
                     that turn to end. A session with shells somewhere else reads its \
                     paths and skills there, so what completes for it is what it can \
                     open.",
                    false,
                    "remote",
                    Some("name"),
                    // Exactly the tables this server was started with: `local` is
                    // what omitting the flag means, and naming it is refused.
                    CompletionSource::Static({
                        // `none` first: it is the value that comes home, and the only one
                        // that is not a table in the config.
                        let mut names = vec!["none".to_string()];
                        names.extend(remotes.to_vec());
                        names
                    }),
                ),
                // SPEC: `[--cwd <cwd>](initial: ".")` -- a session that names
                // none stands where the server stands.
                kind_arg(
                    "cwd",
                    "Working directory for the session: where a new one is created, and \
                     where an existing one is moved to. It must be a directory that \
                     exists on the machine it names -- this one, or the remote's -- or \
                     the command is refused; completion reads that machine's \
                     directories. Omit for the server's own directory.",
                    false,
                    "cwd",
                    Some("path"),
                    // The client lists the filesystem it sits on, which is a hint only.
                    CompletionSource::Paths,
                )
                // SPEC: `--cwd` is `sticky to remote` -- a directory belongs to the
                // machine it exists on.
                .following("remote"),
                arg(
                    "must_create",
                    "`true` refuses the command unless the session does not exist yet, so a \
                     caller that means to start its own session cannot join another one's \
                     transcript by picking a name that was already taken.",
                    false,
                    Some("true|false"),
                    CompletionSource::Static(
                        protocol::SHOW_VALUES
                            .iter()
                            .map(|s| s.to_string())
                            .collect(),
                    ),
                ),
            ],
        },
        CommandSpec {
            target: CommandTarget::Server,
            name: "list_paths".into(),
            description: "List one directory on a remote, for a front-end completing `--cwd`. \
                      Not a tool the model needs: the paths of a session's own shell are \
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
            description: "Delete a session, its shells and its transcript.".into(),
            args: vec![kind_arg(
                "name",
                "Session to delete.",
                true,
                "session",
                Some("name"),
                CompletionSource::Sessions,
            )],
        },
        CommandSpec {
            target: CommandTarget::Server,
            name: "interrupt_session".into(),
            description:
                "Stop the turn one session is running. The turn is dropped, not resumed on \
                      the next start. Detached jobs keep running."
                    .into(),
            args: vec![{
                let mut spec = pos(
                    "session",
                    "Session whose turn should stop.",
                    true,
                    Some("name"),
                    CompletionSource::Sessions,
                );
                // Of the sticky entry `session`, like every session argument.
                spec.kind = Some("session".into());
                spec
            }],
        },
        CommandSpec {
            target: CommandTarget::Server,
            name: "cancel".into(),
            description: "Stop running turns, save what is left undone, and shut the server down. \
                      Detached jobs are left running."
                .into(),
            args: vec![],
        },
        CommandSpec {
            target: CommandTarget::Server,
            name: "events".into(),
            description: "Read the server's event log: one row per thing that happened, oldest \
                      first, as `{id, kind, payload}`. A cursor, not a transcript -- keep the \
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
                      armed. It answers at once -- it does not wait for a match. A match \
                      notices the session that armed it, and runs `--bash`/`--python` when \
                      one is given; the agent's side of waiting for the notice is the \
                      `wait` tool."
                .into(),
            args: vec![
                pos(
                    "subscription",
                    "The name it is armed and dropped by. Arming a name again replaces \
                     what it held.",
                    true,
                    Some("name"),
                    CompletionSource::None,
                ),
                pos(
                    "pattern",
                    "A regex, matched against an event's name -- a log row's `kind` \
                     (`turn_end`, `tool_result`, `context_notice`) or another event's own \
                     (`status`, `usage`, `sessions`, `log`, `cmd_result`) -- and against \
                     `name:session` when the event is about a session, so `turn_end:child` \
                     is one session's turn ending. Unanchored, so write `^turn_end$` for \
                     exactly one name.",
                    true,
                    Some("pattern"),
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
                arg(
                    "bash",
                    "The name of the session's own persistent shell the action runs in: \
                     the same shell `bash` uses under that name, so `cd` and exports are \
                     shared between a subscription and the session.",
                    false,
                    Some("name"),
                    CompletionSource::None,
                ),
                arg(
                    "cmd",
                    "The bash command `--bash` runs on a match; what it prints is a log \
                     row (`subscription`).",
                    false,
                    Some("cmd"),
                    CompletionSource::None,
                ),
                arg(
                    "python",
                    "The name of the session's own persistent interpreter the action runs \
                     in: the same namespace `python` uses under that name.",
                    false,
                    Some("name"),
                    CompletionSource::None,
                ),
                arg(
                    "code",
                    "The Python code `--python` runs on a match; what it prints is a log \
                     row (`subscription`).",
                    false,
                    Some("code"),
                    CompletionSource::None,
                ),
            ],
        },
        CommandSpec {
            target: CommandTarget::Server,
            name: "del_subscribe".into(),
            description: "Drop one named subscription the caller session armed.".into(),
            args: vec![pos(
                "subscription",
                "The name it was armed by.",
                true,
                Some("name"),
                CompletionSource::None,
            )],
        },
        CommandSpec {
            target: CommandTarget::Server,
            name: "trigger_event".into(),
            description: "Publish one event under a chosen name, so the subscriptions whose \
                      pattern matches it fire. The row lands in the event log like any \
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
            name: "get_messages".into(),
            description: "Read the transcript: a range of sequence numbers, oldest first. \
                     Each row arrives as the event it was published as, before the result."
                .into(),
            args: vec![
                pos(
                    "range",
                    "Which rows, as a closed range of sequence numbers: `-1` the newest \
                     row, `-5:` the newest five, `5:10` rows 5 through 10, `:10` the first \
                     ten, `5:` from 5 on. A negative number counts from the end; both ends \
                     inclusive, a missing end means the newest, and a range past the end is \
                     just the rows that exist. Omitted is the whole transcript.",
                    false,
                    Some("begin:end"),
                    CompletionSource::None,
                ),
                kind_arg(
                    "session",
                    "Session to read.",
                    true,
                    "session",
                    Some("name"),
                    CompletionSource::Sessions,
                ),
                arg(
                    "replay",
                    "`true` sends the transcript as a window draws it, and says which \
                     point of the stream the answer is a picture of: the rows a window \
                     shows, the operator's own messages as their own lines, and the \
                     answer still being written. A window that reconnected asks this \
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
                      at the same `seq`. The copy reads exactly as the source does; a turn \
                      it runs later is its own."
                .into(),
            args: vec![
                kind_arg(
                    "session",
                    "The session to copy. It keeps its own rows: a clone reads the source, \
                     it does not take anything away.",
                    true,
                    "session",
                    Some("name"),
                    CompletionSource::Sessions,
                ),
                pos(
                    "name",
                    "The name the copy takes. It must be free: a copy that joined an \
                     existing transcript would be two conversations under one name.",
                    true,
                    Some("name"),
                    CompletionSource::None,
                ),
            ],
        },
    ]
}

/// A flag argument, spelled `--name <value>`.
fn arg(
    name: &str,
    description: &str,
    required: bool,
    value_hint: Option<&str>,
    completions: CompletionSource,
) -> protocol::ArgSpec {
    sticky_arg(
        arg_spec(
            name,
            description,
            required,
            false,
            None,
            value_hint,
            completions,
        ),
        false,
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
    sticky_arg(
        arg_spec(
            name,
            description,
            required,
            true,
            None,
            value_hint,
            completions,
        ),
        false,
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
        name: name.into(),
        description: description.into(),
        required,
        positional,
        kind: kind.map(str::to_string),
        value_hint: value_hint.map(str::to_string),
        completions,
        accepts: None,
        sticky_to: None,
        sticky: false,
    }
}

/// SPEC's `(sticky)`: a value the caller set once, applied to every later command.
fn sticky_arg(mut spec: protocol::ArgSpec, sticky: bool) -> protocol::ArgSpec {
    spec.sticky = sticky;
    spec
}

/// Runs one command, on behalf of whoever `origin` names.
///
/// # Invariant
///
/// `origin` is not the `session` argument: that one is who the command acts on,
/// while the origin is who asked. `subscribe` needs the session the command was
/// run *from*, and `cancel` needs to be able to say who asked, which is the
/// difference between a window's own goodbye and somebody else's.
pub async fn dispatch(d: &Dispatch, name: &str, args: &Args, origin: &Origin) -> Outcome {
    // One catalog decides existence, runner and required args, for every kind.
    let spec = match resolve(&d.catalog().await, name, args) {
        Ok(spec) => spec,
        Err(refusal) => return refusal,
    };
    if spec.target == CommandTarget::ServerScript {
        return d.scripts.run(name, args).await;
    }
    match name {
        "send_message" => send_message(d, args).await,
        "set_session" => set_session(d, args).await,
        "del_session" => del_session(d, args).await,
        "list_paths" => list_paths(d, args).await,
        "interrupt_session" => interrupt_session(d, args).await,
        "cancel" => cancel(d, origin).await,
        "get_messages" => get_messages(d, args).await,
        "clone_session" => clone_session(d, args).await,
        "events" => events(d, args).await,
        "subscribe" => subscribe(d, args, origin.session()).await,
        "del_subscribe" => del_subscribe(d, args, origin.session()).await,
        "trigger_event" => trigger_event(d, args).await,
        // Every tool is a command: the registry decides what exists.
        name if d.agent.tools.get(name).is_some() => {
            crate::tool_cmd::dispatch_tool(d, name, args).await
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
    /// Every command offered, from all three sources: the only list there is.
    pub async fn catalog(&self) -> Vec<CommandSpec> {
        let mut all = server_catalog(&self.sessions.remote_names());
        all.extend(crate::tool_cmd::catalog(&self.agent.tools));
        all.extend(self.scripts.commands().await);
        all
    }
}

async fn list_paths(d: &Dispatch, args: &Args) -> Outcome {
    let remote = args.get("remote").map(String::as_str);
    let path = args.get("path").map(String::as_str).unwrap_or("");
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

async fn send_message(d: &Dispatch, args: &Args) -> Outcome {
    let Some(message) = args.get("message").cloned() else {
        return Outcome::err("send_message requires a message");
    };
    let Some(name) = args.get("name").cloned() else {
        return Outcome::err(
            "send_message requires a session; set the sticky `session` entry to avoid \
             typing it",
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
    let llm = llm.as_ref().map(|(client, _)| client.as_ref());
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
    let epoch = session.cancel_epoch();
    if let Some(gate) = session.try_claim() {
        // The gate is claimed before the durable write, so a second sender can only
        // inject, never spawn a competing turn.
        match d.store.begin_turn(session.id(), name, &message) {
            Ok((task, seq)) => {
                // A row like any other: windows draw it from their own echo.
                d.agent.announce_row(name, Ok(seq));
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

async fn set_session(d: &Dispatch, args: &Args) -> Outcome {
    let name = args["name"].clone();
    // A leading `@` is the harness's own namespace: a name a person could take
    // is a name the harness cannot trust.
    if let Some(refusal) = crate::monitor::reserved(&name) {
        return Outcome::err(refusal);
    }
    // Then whether the name is a name at all: the list draws one row per prefix.
    if let Some(refusal) = crate::session::checked_name(&name) {
        return Outcome::err(refusal);
    }
    // A caller that means to start its own session says so: a name collision
    // must never select another one's transcript.
    match args.get("must_create") {
        None => {}
        Some(raw) => match parse_bool(raw) {
            Ok(false) => {}
            Ok(true) => {
                if d.store.get_session(&name).ok().flatten().is_some() {
                    return Outcome::err(format!(
                        "a session named {name} already exists, and --must_create refuses to \
                         join it; pick a name of your own"
                    ));
                }
            }
            Err(e) => return Outcome::err(e),
        },
    }
    // Resolve `--model` now: a typo should fail the command that made it.
    let stated = match args.get("model") {
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
    let threshold = match args.get("ctx_limit") {
        None => None,
        Some(raw) => match parse_percent(raw) {
            Ok(v) => Some(v),
            Err(e) => return Outcome::err(e),
        },
    };
    // Absent, stated, stated-as-none: three answers, and only the last sends none.
    let cache_tier: Option<Option<String>> = match args.get("cache_tier").map(String::as_str) {
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
    let cache_keepalive = match args.get("cache_keepalive").map(String::as_str) {
        None => None,
        Some(raw) => match parse_bool(raw) {
            Ok(value) => Some(value),
            Err(e) => return Outcome::err(format!("--cache_keepalive: {e}")),
        },
    };
    // A percentage needs a window; silently keeping the old budget would lie.
    let context = entry.context.or(d.agent.default_context);
    if let (Some(percent), None) = (threshold, context) {
        return Outcome::err(format!(
            "--ctx_limit {percent}% needs a context window to be a percentage of, and \
             {} is not in the roster with a `context`",
            entry.model
        ));
    }
    // A session that exists is moved; one that does not is created where it says.
    // Asking where it already is costs nothing.
    let stated_cwd = args.get("cwd").map(String::as_str);
    // Three answers, not two: the flag was left out, it names this machine, or it
    // names a remote. See `session::RemoteAsk`.
    let asked_remote = match args.get("remote").map(String::as_str) {
        None => crate::session::RemoteAsk::Keep,
        Some(stated) => crate::session::RemoteAsk::Machine(crate::session::remote_request(stated)),
    };
    let (session, moved) = match d.store.get_session(&name).ok().flatten() {
        Some(row) => {
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
            if let Some(stated) = stated_cwd {
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
            match d
                .sessions
                .ensure_on(&name, stated_cwd, Some(&entry.model), lands.as_deref())
            {
                Ok(session) => (session, None),
                Err(e) => return Outcome::err(e.to_string()),
            }
        }
    };

    let mut tuning = session.tuning();
    let mut changed = false;
    // Resolved here and stored, never inferred at display time, so the title and
    // the request read one word.
    let level = match args.get("thinking").map(String::as_str) {
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
    if changed || cache_changed {
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
        session.set_tuning(tuning.clone());
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

/// A flag's value, or what is wrong with it: never a silent default.
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

async fn get_messages(d: &Dispatch, args: &Args) -> Outcome {
    let name = args["session"].clone();
    let session = match d.sessions.get(&name) {
        Ok(s) => s,
        Err(e) => return Outcome::err(e.to_string()),
    };
    let replay = matches!(
        args.get("replay").map(String::as_str),
        Some("true") | Some("1")
    );
    if replay {
        return replay_transcript(d, &name, &session, args);
    }
    let last_seq = match d.store.last_seq(session.id()) {
        Ok(v) => v,
        Err(e) => return Outcome::err(e.to_string()),
    };
    let window = match args.get("range") {
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
        let window = match args.get("range") {
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
        Ok::<_, anyhow::Error>((rows, session.open_parts(), window.first))
    });
    let (read, open, first) = match read {
        Ok(t) => t,
        Err(e) => return Outcome::err(e.to_string()),
    };
    let mut events: Vec<Event> = read
        .messages
        .iter()
        .filter_map(|row| row_event(name, row, true))
        .collect();
    // Published and not yet stored: what a window that reconnected has no other copy of.
    for part in &open {
        if part.text.trim().is_empty() {
            continue;
        }
        events.push(Event::Response(ResponseEvent {
            session: name.to_string(),
            turn: part.turn,
            part: part.part,
            text: part.text.clone(),
            done: false,
            call_id: part.call_id.clone(),
        }));
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
    let source = args["session"].clone();
    let name = args["name"].clone();
    let report = match d.store.clone_session(&source, &name) {
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

/// Arms a pattern, and answers with what it armed.
///
/// # Invariant
///
/// The session is the caller's, never the `--event` pattern's: a pattern names
/// what to watch, and the caller is who hears about it. Refusing without a caller
/// is deliberate -- a subscription with no addressee would match, fire, and tell
/// nobody, which is the one failure a registry cannot notice by itself.
async fn subscribe(d: &Dispatch, args: &Args, caller: Option<&str>) -> Outcome {
    let name = args
        .get("subscription")
        .map(String::as_str)
        .unwrap_or("")
        .trim();
    if name.is_empty() {
        return Outcome::err("subscribe needs a name: `subscribe <name> <pattern>`");
    }
    let pattern = args.get("pattern").map(String::as_str).unwrap_or("").trim();
    if pattern.is_empty() {
        return Outcome::err(
            "subscribe needs a pattern over event names, e.g. `turn_end`".to_string(),
        );
    }
    let Some(caller) = caller.filter(|name| !name.trim().is_empty()) else {
        return Outcome::err(
            "subscribe notices the session it was run from, and this client did not say \
             which: run it from a session's shell, where `$GONGGA_SESSION` is set",
        );
    };
    match d.store.get_session(caller) {
        Ok(Some(_)) => {}
        Ok(None) => return Outcome::err(format!("no session named {caller:?} to notice")),
        Err(e) => return Outcome::err(e.to_string()),
    }
    let persistent = match args.get("persistant") {
        None => false,
        Some(raw) => match parse_bool(raw) {
            Ok(value) => value,
            Err(_) => {
                return Outcome::err(format!(
                    "--persistant takes {}, not {raw:?}",
                    protocol::SHOW_VALUES.join(" or ")
                ));
            }
        },
    };
    let action = match (
        args.get("bash"),
        args.get("cmd"),
        args.get("python"),
        args.get("code"),
    ) {
        (Some(_), _, Some(_), _) | (_, Some(_), _, Some(_)) => {
            return Outcome::err("subscribe takes one of --bash/--cmd or --python/--code");
        }
        (Some(bash), Some(cmd), None, None) => Some(crate::subscriptions::Action::Bash {
            name: bash.clone(),
            cmd: cmd.clone(),
        }),
        (None, None, Some(python), Some(code)) => Some(crate::subscriptions::Action::Python {
            name: python.clone(),
            code: code.clone(),
        }),
        (Some(bash), None, None, None) => {
            return Outcome::err(format!(
                "--bash names the shell; --cmd is what runs in it: `subscribe {name}                  <pattern> --bash {bash} --cmd <cmd>`"
            ));
        }
        (None, None, Some(python), None) => {
            return Outcome::err(format!(
                "--python names the interpreter; --code is what runs in it: `subscribe                  {name} <pattern> --python {python} --code <code>`"
            ));
        }
        // SPEC leaves it optional: with no action a match does nothing. Warned.
        (None, None, None, None) => {
            tracing::warn!(
                session = %caller, name, pattern = %pattern,
                "subscription armed with no action: a match will do nothing"
            );
            None
        }
        // --cmd/--code without the interpreter they run in.
        _ => return Outcome::err("subscribe: --cmd needs --bash, and --code needs --python"),
    };
    match d
        .subscriptions
        .arm(caller, name, pattern, persistent, action)
        .await
    {
        Ok(row) => {
            tracing::info!(
                session = %caller, subscription = row.id, name, pattern = %pattern,
                persistent, "subscribed"
            );
            Outcome::ok_with(
                format!(
                    "subscription `{name}` armed: a match of `{pattern}` runs its command in \
                     {caller}'s shell{}",
                    if persistent { ", every time" } else { " once" }
                ),
                json!({
                    "id": row.id,
                    "session": caller,
                    "subscription": name,
                    "event": pattern,
                    "persistant": persistent,
                }),
            )
        }
        Err(e) => Outcome::err(e),
    }
}

/// Drops one named subscription; the caller drops its own, never another session's.
async fn del_subscribe(d: &Dispatch, args: &Args, caller: Option<&str>) -> Outcome {
    let name = args
        .get("subscription")
        .map(String::as_str)
        .unwrap_or("")
        .trim();
    let Some(caller) = caller.filter(|name| !name.trim().is_empty()) else {
        return Outcome::err(
            "del_subscribe drops what a session armed, and this client did not say which: \
             run it from a session's shell, where `$GONGGA_SESSION` is set",
        );
    };
    match d.subscriptions.disarm_name(caller, name).await {
        Ok(0) => Outcome::err(format!("no subscription named {name:?} for {caller}")),
        Ok(n) => Outcome::ok(format!("subscription `{name}` dropped ({n})")),
        Err(e) => Outcome::err(e),
    }
}

/// Publishes one event under a chosen name, as a log row every matcher sees.
async fn trigger_event(d: &Dispatch, args: &Args) -> Outcome {
    let event = args.get("event").map(String::as_str).unwrap_or("").trim();
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
async fn events(d: &Dispatch, args: &Args) -> Outcome {
    let after = match args.get("since").map(String::as_str) {
        None => 0,
        Some(raw) => match raw.trim().parse::<i64>() {
            Ok(n) if n >= 0 => n,
            _ => return Outcome::err(format!("since wants a log id, not {raw:?}")),
        },
    };
    let kind = args
        .get("kind")
        .map(String::as_str)
        .filter(|k| !k.is_empty());
    let session = match args.get("session").map(String::as_str) {
        None => None,
        Some(name) => match d.sessions.get(name) {
            Ok(s) => Some(s.id()),
            Err(e) => return Outcome::err(e.to_string()),
        },
    };
    let limit = match args.get("limit").map(String::as_str) {
        None => 200,
        Some(raw) => match raw.trim().parse::<u32>() {
            Ok(n) if n > 0 => n.min(5000),
            _ => return Outcome::err(format!("limit wants a count above zero, not {raw:?}")),
        },
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
        events.push(Event::Record {
            id: row.id,
            seq: row.seq,
            session: row.session_id.and_then(|id| names.get(&id).cloned()),
            kind: row.kind.clone(),
            payload,
            at: row.created_at,
        });
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
        .filter_map(|row| row_event(name, row, false))
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

fn prepare_delete(d: &Dispatch, name: &str) -> (bool, usize) {
    let interrupted = interrupt(d, name);
    if let Some(session) = d.sessions.peek(name) {
        session.mark_deleted();
    }
    interrupted
}

async fn finish_delete(d: &Dispatch, name: &str, was_busy: bool, dropped: usize) -> Outcome {
    let session = d.sessions.peek(name);
    let _gate = match &session {
        Some(session) => Some(session.claim().await),
        None => None,
    };
    let stopped = d.sessions.detached().stop_session(name).await;
    if stopped.failed > 0 {
        return Outcome::err(format!(
            "could not stop {} detached bash(es) for {name}; session kept for retry",
            stopped.failed
        ));
    }
    if let Some(session) = &session {
        session.close_shells().await;
    }
    match d.sessions.delete(name) {
        Ok(true) => {
            // SPEC: a deleted session's bash go with it, and so do its
            // subscriptions -- there is nobody left to tell.
            let forgotten = d.subscriptions.forget_session(name).await;
            publish_sessions(d);
            let mut message = format!("deleted session {name}");
            if stopped.stopped > 0 {
                message.push_str(&format!(
                    " and stopped {} detached bash(es)",
                    stopped.stopped
                ));
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
            Outcome::ok(message)
        }
        Ok(false) => Outcome::err(format!("no session named {name:?}")),
        Err(e) => Outcome::err(e.to_string()),
    }
}

async fn del_session(d: &Dispatch, args: &Args) -> Outcome {
    let name = args["name"].clone();
    let (was_busy, dropped) = prepare_delete(d, &name);
    finish_delete(d, &name, was_busy, dropped).await
}

/// Stops one session's turn and drops its pending rows, unlike `cancel`.
fn interrupt(d: &Dispatch, name: &str) -> (bool, usize) {
    let Some(session) = d.sessions.peek(name) else {
        return (false, 0);
    };
    // Read before the cancel: afterwards there is nothing left to ask.
    let was_busy = session.busy();
    session.cancel();
    // Not `request_stop`: aborting is deliberate, so the work is not owed.
    let dropped = d.store.clear_pending_tasks(name).unwrap_or(0);
    (was_busy, dropped)
}

async fn interrupt_session(d: &Dispatch, args: &Args) -> Outcome {
    let name = args["session"].clone();
    // `peek`, not `get`: materialising would start shells just to stop nothing.
    if d.sessions.peek(&name).is_none() {
        return match d.store.get_session(&name) {
            Ok(Some(_)) => Outcome::ok(format!("{name} is not running; nothing to interrupt")),
            Ok(None) => Outcome::err(format!("no session named {name:?}")),
            Err(e) => Outcome::err(e.to_string()),
        };
    }
    let (was_busy, dropped) = interrupt(d, &name);
    let mut message = if was_busy {
        format!("interrupted {name}")
    } else {
        format!("{name} is idle")
    };
    if dropped > 0 {
        message.push_str(&format!("; dropped {dropped} pending task(s)"));
    }
    Outcome::ok(message)
}

/// The SPEC's `cancle`: stop turns and leave their work for the next start.
///
/// # Invariant
///
/// Detached jobs keep running: they were started precisely so they would
/// outlive this.
/// A stop asked from outside the protocol -- a signal -- says what `cancel` says,
/// and records the same way which ask it was.
pub(crate) async fn stop(d: &Dispatch) {
    // A window that cancelled as it left has already asked; this is the echo.
    if d.shutting_down.load(Ordering::SeqCst) {
        return;
    }
    let reason = crate::signal::reason()
        .unwrap_or("a signal reached the server")
        .to_string();
    let said = cancel(d, &Origin::Signal { reason }).await;
    tracing::info!(answer = %said.message, "stopped as a signal asks");
}

/// Stops every turn, records who asked, and leaves the work for the next start.
///
/// # Invariant
///
/// The record comes before the answer: it is how a window that did *not* ask
/// learns the server is stopping, and its `payload.by` is the only place that
/// says who asked. The status that follows from it states the fact, not the
/// name -- see `docs/notes/server-commands.md`.
async fn cancel(d: &Dispatch, origin: &Origin) -> Outcome {
    // First: the listen loop keeps dispatching through the notice window; a turn
    // started there is not waited for.
    d.shutting_down.store(true, Ordering::SeqCst);
    let running = d.sessions.live_sessions();
    for session in &running {
        // A stop, not a kill: SPEC finishes what cannot be serialized first.
        session.request_stop();
    }
    let detached = d.sessions.detached().count().await;
    let pending = d.store.pending_tasks().map(|t| t.len()).unwrap_or(0);
    let by = origin.describe();
    d.agent.log_event(
        None,
        protocol::SERVER_STOP,
        &json!({
            "by": by,
            "sessions": running.len(),
            "pending": pending,
            "detached": detached,
        }),
    );
    // Wait before shutting down: the acknowledgement is published after this returns.
    let shutdown = d.shutdown.clone();
    tokio::spawn(async move {
        tokio::time::sleep(SHUTDOWN_NOTICE).await;
        shutdown.cancel();
    });
    let said = format!(
        "cancelling {} session(s); {} pending task(s) will be picked up on the next start; {} \
         detached bash(es) keep running -- they outlive the server, stop them with `del_bash`",
        running.len(),
        pending,
        detached
    );
    tracing::warn!(
        running = running.len(),
        by = %by,
        "the server is stopping: it keeps answering until its turns finish"
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

/// Whether this row is the server speaking rather than a person waiting.
///
/// # Invariant
///
/// A notice about a background job is news for a working session; on its own it
/// must not cost an idle session a turn. See `docs/notes/server-commands.md`.
fn is_notice(task: &PendingTask) -> bool {
    task.payload.get("notice").is_some()
}

/// The rows a session owes a *turn* for.
///
/// # Invariant
///
/// An operator message or a turn begun; a notice alone waits for the next step
/// boundary.
fn owes_a_turn(tasks: &[PendingTask]) -> bool {
    tasks
        .iter()
        .any(|task| task.kind == "turn" || (task.kind == "inject" && !is_notice(task)))
}

/// Wakes each session with stranded work at most once.
pub fn nudge_inboxes(d: &Dispatch) -> usize {
    if d.shutting_down.load(Ordering::SeqCst) || d.shutdown.is_cancelled() {
        return 0;
    }
    let Ok(tasks) = d.store.pending_tasks() else {
        return 0;
    };
    let mut started = 0;
    for (name, tasks) in recovery_groups(tasks) {
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
        // A notice about a shell the last run took names a buffer nothing can
        // read: delivering it is a turn on stale news.
        let mut kept = Vec::with_capacity(tasks.len());
        let mut dropped = 0;
        for task in tasks {
            if task.payload["notice"] == json!("background") {
                let _ = d.store.delete_pending_task(task.id);
                dropped += 1;
                continue;
            }
            kept.push(task);
        }
        if dropped > 0 {
            tracing::info!(
                session = %name, dropped,
                "dropped notices about shells the last run took with it"
            );
        }
        let owed = owes_a_turn(&kept);
        let owned = kept
            .iter()
            .filter(|task| task.kind == "turn")
            .map(|task| task.id)
            .collect();
        if let Some(work) = claim_recovery(d, session, owned, owed) {
            tracing::info!(session = %name, "replaying work left pending");
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
        context_limit: tuning.context_limit.or(Some(d.agent.context_limit_percent)),
    }
}
