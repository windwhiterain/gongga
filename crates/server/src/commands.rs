//! The command surface: catalog and dispatch.
//! See `docs/notes/server-commands.md`.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use protocol::{
    Args, CommandSpec, CommandTarget, CompletionSource, Event, LogLevel, REPLAY_TURN,
    ResponseEvent, ResponsePart, ToolAnswerEvent, ToolStream,
};
use serde_json::json;
use std::time::Duration;

use tokio_util::sync::CancellationToken;

use crate::agent::Agent;
use crate::bus::EventBus;
use crate::session::SessionManager;
use crate::store::{MessageRow, PendingTask, Store, Transcript};

/// How long `cancel` waits before pulling the plug, so the acknowledgement
/// still reaches whoever asked for it.
const SHUTDOWN_NOTICE: Duration = Duration::from_millis(250);

pub struct Dispatch {
    pub agent: Arc<Agent>,
    pub sessions: SessionManager,
    pub store: Store,
    pub bus: EventBus,
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
}

/// Owns one turn's slot in [`Dispatch::active`], releasing it even on a panic.
struct ActiveGuard(Arc<AtomicUsize>);

impl ActiveGuard {
    fn new(active: Arc<AtomicUsize>) -> Self {
        active.fetch_add(1, Ordering::SeqCst);
        Self(active)
    }
}

impl Drop for ActiveGuard {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
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
    tokio::spawn(async move {
        let _active = active;
        let session = work.session.clone();
        agent
            .run_turn(&work.session, work.tasks, work.epoch, work.gate)
            .await;
        // What the turn queued behind itself is this turn's to finish, and this is
        // the one moment the gate is free without being taken: `set_session` from
        // the agent's own shell lands here.
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
pub fn server_catalog(targets: &[String]) -> Vec<CommandSpec> {
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
                arg(
                    "session",
                    "Target session. Required: there is no implicit default.",
                    true,
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
                arg(
                    "session",
                    "Session to create or reconfigure.",
                    true,
                    Some("name"),
                    CompletionSource::Sessions,
                ),
                arg(
                    "cwd",
                    "Working directory for the session: where a new one is created, and \
                     where an existing one is moved to. For a session on a target this is \
                     a path on that machine, which the endpoint canonicalises and refuses \
                     when it does not exist; completion reads that machine's directories.",
                    false,
                    Some("path"),
                    // The client lists the filesystem it sits on, which is a hint only.
                    CompletionSource::Paths,
                ),
                // SPEC: `--model (sticky)`, so a model set once is a mode rather
                // than a repetition in every command.
                sticky_arg(
                    arg_spec(
                        "model",
                        "The model to run this session's turns on, as <provider>/<model \
                         name>. Remembered: a later `set_session` that names none uses it.",
                        false,
                        Some("model"),
                        CompletionSource::Static(crate::providers::roster().models()),
                    ),
                    true,
                ),
                arg(
                    "context_limit",
                    "The context level at which this session gets a `Context notice`, e.g. \
                     `30%`. 0 never notices. Defaults to a share of the \
                     model's context window, or the server's token budget when the \
                     window is unknown.",
                    false,
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
                ),
                arg(
                    "target",
                    "Which machine this session's shells run on, named by a `[targets.<name>]` \
                     table in the server's config. Omit for this machine. Naming a \
                     different one moves an existing session: every shell on the old \
                     machine is ended first, and the move is refused while a turn runs.",
                    false,
                    Some("name"),
                    // Exactly the tables this server was started with: `local` is
                    // what omitting the flag means, and naming it is refused.
                    CompletionSource::Static(targets.to_vec()),
                ),
                arg(
                    "thinking_effort",
                    "How hard the model should think, in the roster's spelling (`low`, `high`, \
                     `xhigh`, ...). Mapped onto what the model actually accepts and checked \
                     against it, so a level it does not take fails here. Defaults to the \
                     model's own.",
                    false,
                    Some("level"),
                    CompletionSource::Static(
                        ["none", "minimal", "low", "medium", "high", "xhigh", "max"]
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
            description: "List one directory on a target, for a front-end completing `--cwd`. \
                      Not a tool the model needs: the paths of a session's own shell are \
                      read by running `ls` in it."
                .into(),
            args: vec![
                arg(
                    "target",
                    "The target whose filesystem to read. Omitted is this machine.",
                    false,
                    Some("name"),
                    CompletionSource::Static(targets.to_vec()),
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
            args: vec![arg(
                "session",
                "Session to delete.",
                true,
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
            args: vec![arg(
                "session",
                "Session whose turn should stop.",
                true,
                Some("name"),
                CompletionSource::Sessions,
            )],
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
            name: "get_messages".into(),
            description: "Read the transcript: a range of sequence numbers, oldest first. \
                     Each row arrives as the event it was published as, before the result."
                .into(),
            args: vec![
                arg(
                    "session",
                    "Session to read.",
                    true,
                    Some("name"),
                    CompletionSource::Sessions,
                ),
                arg(
                    "range",
                    "Which rows, as a closed range of sequence numbers: `-1` the newest row, \
                     `-5:` the newest five, `5:10` rows 5 through 10, `:10` the first ten, `5:` \
                     from 5 on. A negative number counts from the end; both ends inclusive, and \
                     a range past the end is just the rows that exist.",
                    true,
                    Some("slice"),
                    CompletionSource::None,
                ),
            ],
        },
    ]
}

fn arg(
    name: &str,
    description: &str,
    required: bool,
    value_hint: Option<&str>,
    completions: CompletionSource,
) -> protocol::ArgSpec {
    sticky_arg(
        arg_spec(name, description, required, value_hint, completions),
        false,
    )
}

fn arg_spec(
    name: &str,
    description: &str,
    required: bool,
    value_hint: Option<&str>,
    completions: CompletionSource,
) -> protocol::ArgSpec {
    protocol::ArgSpec {
        name: name.into(),
        description: description.into(),
        required,
        value_hint: value_hint.map(str::to_string),
        completions,
        sticky: false,
    }
}

/// SPEC's `(sticky)`: a value the caller set once, applied to every later command.
fn sticky_arg(mut spec: protocol::ArgSpec, sticky: bool) -> protocol::ArgSpec {
    spec.sticky = sticky;
    spec
}

pub async fn dispatch(d: &Dispatch, name: &str, args: &Args) -> Outcome {
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
        "cancel" => cancel(d).await,
        "get_messages" => get_messages(d, args).await,
        "events" => events(d, args).await,
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
        let mut all = server_catalog(&self.sessions.target_names());
        all.extend(protocol::client_commands());
        all.extend(self.scripts.commands().await);
        all
    }
}

async fn list_paths(d: &Dispatch, args: &Args) -> Outcome {
    let target = args.get("target").map(String::as_str);
    let path = args.get("path").map(String::as_str).unwrap_or("");
    match d.sessions.list_paths(target, path).await {
        Ok(entries) => {
            let rows: Vec<serde_json::Value> = entries
                .into_iter()
                .map(|(name, directory)| json!({"name": name, "dir": directory}))
                .collect();
            let count = rows.len();
            Outcome::ok_with(
                format!("{count} entries in {path:?}"),
                json!({"path": path, "target": target, "entries": rows}),
            )
        }
        Err(e) => Outcome::err(e.to_string()),
    }
}

async fn send_message(d: &Dispatch, args: &Args) -> Outcome {
    let Some(message) = args.get("message").cloned() else {
        return Outcome::err("send_message requires --message");
    };
    let Some(name) = args.get("session").cloned() else {
        return Outcome::err(
            "send_message requires --session; set a sticky `session` to avoid typing it",
        );
    };
    deliver(d, &name, message).await
}

/// Resolves a message's `@image:` and `@skill:` references before it is stored.
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
    let on_endpoint = d
        .sessions
        .recorded_target(session.name())
        .is_some_and(|target| target != "local");
    // These resolve on the peer, so they are refused here; `@file:` stays literal.
    if on_endpoint {
        let named: Vec<String> = crate::meta::mentions(&message)
            .into_iter()
            .filter(|m| m.kind != protocol::Reference::File)
            .map(|m| m.value)
            .collect();
        if !named.is_empty() {
            return Err(format!(
                "this session runs on a remote endpoint, and {} cannot be resolved here:                  the file it names is on that machine. Send the text without the reference,                  or name the path for the endpoint to read itself.",
                named.join(", ")
            ));
        }
        return Ok(crate::meta::Expanded {
            text: message,
            images: Vec::new(),
        });
    }
    let llm = d.agent.route(session.name(), &session.tuning()).ok();
    let llm = llm.as_ref().map(|(client, _)| client.as_ref());
    let prompt = d.store.prompt(session.id()).ok().flatten();
    let indexed = crate::meta::literal(
        &session.cwd(),
        &message,
        prompt.as_ref().map(|prompt| prompt.rooted.as_slice()),
    );
    let borrowed: Vec<&str> = indexed.iter().map(String::as_str).collect();
    Ok(crate::meta::expand(&session.cwd(), &message, llm, &borrowed).await)
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
            Ok(task) => {
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
    let name = args["session"].clone();
    // A leading `@` is the harness's own namespace: SPEC's monitor lives there,
    // and a name a person could take is a name the harness cannot trust.
    if let Some(refusal) = crate::monitor::reserved(&name) {
        return Outcome::err(refusal);
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
                    // Same sentence the catalog would give a missing argument: this
                    // is `--model`, required only when nothing else names a model.
                    return Outcome::err(missing_arguments("set_session", &["model".to_string()]));
                }
                match resolve_model(d, stated_default) {
                    Ok(entry) => entry,
                    Err(e) => return Outcome::err(format!("DEFAULT_SESSION_MODEL: {e}")),
                }
            }
        },
    };
    let threshold = match args.get("context_limit") {
        None => None,
        Some(raw) => match parse_percent(raw) {
            Ok(v) => Some(v),
            Err(e) => return Outcome::err(e),
        },
    };
    // A percentage needs a window; silently keeping the old budget would lie.
    let context = entry.context.or(d.agent.default_context);
    if let (Some(percent), None) = (threshold, context) {
        return Outcome::err(format!(
            "--context_limit {percent}% needs a context window to be a percentage of, and \
             {} is not in the roster with a `context`",
            entry.model
        ));
    }
    // A session that exists can be moved; one that does not is created where it
    // says. Asking for where it already is costs nothing: no shell is torn down for
    // a `--cwd` that names the directory it is standing in.
    let stated_cwd = args.get("cwd").map(String::as_str);
    let stated_target = args.get("target").map(String::as_str);
    let (session, moved) = match d.store.get_session(&name).ok().flatten() {
        Some(row) => {
            let live = d.sessions.peek(&name);
            let here = live
                .as_ref()
                .map(|session| session.cwd())
                .unwrap_or_else(|| std::path::PathBuf::from(&row.cwd));
            let recorded_target = row.target.clone().unwrap_or_else(|| "local".to_string());
            let wanted_target = stated_target
                .map(str::trim)
                .filter(|asked| !asked.is_empty())
                .map(str::to_string);
            let target_moves = wanted_target
                .as_deref()
                .is_some_and(|asked| !same_target(asked, &recorded_target));
            let cwd_moves = stated_cwd.is_some_and(|asked| !same_directory(asked, &here));
            let session = match live {
                Some(session) => session,
                None => match d.sessions.get(&name) {
                    Ok(session) => session,
                    Err(e) => return Outcome::err(e.to_string()),
                },
            };
            if !target_moves && !cwd_moves {
                (session, None)
            } else {
                let moved = match d
                    .sessions
                    .relocate(&session, wanted_target.as_deref(), stated_cwd)
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
            match d
                .sessions
                .ensure_on(&name, stated_cwd, Some(&entry.model), stated_target)
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
    let level = match args.get("thinking_effort") {
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
    if changed {
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
        session.set_tuning(tuning.clone());
    }
    publish_sessions(d);

    let cwd = session.cwd();
    // Confirms the write and stops: what the session runs on is its pane's title.
    let message = match &moved {
        None => format!("session is now {name}"),
        // Waiting for the turn is the normal answer for the one caller that wants a
        // move at all -- the agent, inside the turn it is moving out of.
        Some(crate::session::MoveOutcome::Queued(request)) => format!(
            "session {name} will move to {} {} when its turn ends",
            request
                .target
                .clone()
                .unwrap_or_else(|| "local".to_string()),
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
            // A moved remote shell has not connected yet, so there is no directory
            // to name; saying "at" and then nothing would read as a failure.
            let where_now = if cwd.as_os_str().is_empty() {
                "where its endpoint starts".to_string()
            } else {
                format!("at {}", cwd.display())
            };
            format!(
                "session {name} moved to {} {where_now}, {} shell(s) ended{unproved}",
                d.sessions
                    .recorded_target(&name)
                    .unwrap_or_else(|| "local".to_string()),
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
            "target": d.sessions.recorded_target(&name),
            "model": tuning.model,
            "context_limit": tuning.context_limit,
            "thinking_effort": tuning.thinking_effort,
        }),
    )
}

/// Whether a target a command names is the one the session already records.
///
/// Names as the config spelled them: `local` and no target at all are one place, and
/// a row that records nothing is standing on this machine.
fn same_target(asked: &str, recorded: &str) -> bool {
    let asked = if asked.is_empty() { "local" } else { asked };
    let recorded = if recorded.is_empty() {
        "local"
    } else {
        recorded
    };
    asked == recorded
}

/// Whether two spellings name one directory, so a `--cwd` that agrees is not a move.
///
/// # Invariant
///
/// Comparing paths as strings would refuse `C:/work` against `C:\\work`, which name
/// one directory; this compares what the platform compares.
fn same_directory(written: &str, actual: &std::path::Path) -> bool {
    let spelled = std::path::Path::new(written.trim());
    if spelled == actual {
        return true;
    }
    // A canonicalising comparison would answer "no" for a path that does not
    // exist, so this flattens what the platform ignores: separators, and case
    // where the filesystem ignores it.
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
        // A bare name is not an error the roster should answer twice: the second
        // search is only reached when the first found no such pair.
        Err(e) => d
            .providers
            .resolve_by_name(model)
            .map(|c| c.entry)
            .ok_or_else(|| format!("--model {model}: {e}")),
    }
}

/// `30%`, `30`, or `0` for "never notice"; anything else is refused, not rounded.
fn parse_percent(raw: &str) -> Result<u8, String> {
    let trimmed = raw.trim().trim_end_matches('%').trim();
    let value: u16 = trimmed
        .parse()
        .map_err(|_| format!("--context_limit takes a percentage like `30%`, got {raw:?}"))?;
    if value > 100 {
        return Err(format!(
            "--context_limit is a percentage of the context window, so it cannot exceed 100%; \
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
            format!(
                "--range takes sequence numbers like `5:10` or `-5:`, got {part:?} in the {side}"
            )
        })?;
        // Seq starts at 1, so 0 is a typo written either way.
        if n == 0 {
            return Err(format!(
                "--range counts sequence numbers from 1, or from -1 backwards; got 0 in the {side}"
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
            "--range takes one `start:end` range, not a step: got {raw:?}"
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
                return Err("--range needs a bound on one side, e.g. `-5:` or `:10`".into());
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
                    "--range {raw} runs backwards: it starts at {first} and ends at {last}"
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
    let last_seq = match d.store.last_seq(session.id()) {
        Ok(v) => v,
        Err(e) => return Outcome::err(e.to_string()),
    };
    let raw = args["range"].as_str();
    let window = match parse_seq_slice(raw, last_seq) {
        Ok(w) => w,
        Err(e) => return Outcome::err(e),
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
    // Names for the rows that have one: a reader should not have to hold the
    // session table in its head to know whose `turn_end` it just read. Read once,
    // as a map: a page is up to `limit` rows and one query per row would be silly.
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
        .filter_map(|row| row_event(name, row))
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
fn row_event(name: &str, row: &MessageRow) -> Option<Event> {
    if row.text.trim().is_empty() {
        return None;
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
        // A notice and the prompt in force are the harness, not the model:
        // they travel as a log line.
        _ => {
            return Some(Event::Log {
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
            // What it learnt about its own prefix goes with it: a new session of the
            // same name is a new conversation, and must not be warmed by the old.
            if let Err(e) = d.store.forget_prefix_layout(name) {
                tracing::warn!(session = %name, error = %e, "could not forget the prefix layout");
            }
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
            Outcome::ok(message)
        }
        Ok(false) => Outcome::err(format!("no session named {name:?}")),
        Err(e) => Outcome::err(e.to_string()),
    }
}

async fn del_session(d: &Dispatch, args: &Args) -> Outcome {
    let name = args["session"].clone();
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
    // Not `cancel_and_preserve`: aborting is deliberate, so the work is not owed.
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
/// A stop asked from outside the protocol -- a signal -- says what `cancel` says.
pub(crate) async fn stop(d: &Dispatch) {
    // A window that cancelled as it left has already asked; this is the echo.
    if d.shutting_down.load(Ordering::SeqCst) {
        return;
    }
    let said = cancel(d).await;
    tracing::info!(answer = %said.message, "stopped as a signal asks");
}

async fn cancel(d: &Dispatch) -> Outcome {
    // First: the listen loop keeps dispatching through the notice window; a turn
    // started there is not waited for.
    d.shutting_down.store(true, Ordering::SeqCst);
    let running = d.sessions.live_sessions();
    for session in &running {
        // Preserve, not cancel: "cancel, restart, continue" is the point of this.
        session.cancel_and_preserve();
    }
    let detached = d.sessions.detached().count().await;
    let pending = d.store.pending_tasks().map(|t| t.len()).unwrap_or(0);
    // Wait before shutting down: the acknowledgement is published after this returns.
    let shutdown = d.shutdown.clone();
    tokio::spawn(async move {
        tokio::time::sleep(SHUTDOWN_NOTICE).await;
        shutdown.cancel();
    });
    Outcome::ok(format!(
        "cancelling {} session(s); {} pending task(s) will be picked up on the next start; {} \
         detached bash(es) keep running -- they outlive the server, stop them with `del_bash`",
        running.len(),
        pending,
        detached
    ))
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
/// A notice about a background job is news for a session that is already working;
/// on its own it must not cost an idle session a turn. See
/// `docs/notes/server-commands.md`.
fn is_notice(task: &PendingTask) -> bool {
    task.payload.get("notice").is_some()
}

/// The rows a session owes a *turn* for: an operator message, or a turn already
/// begun. A notice alone is delivered at the next step boundary instead.
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

/// Replays interrupted work with exactly one recovery owner per session.
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
        // A notice about a shell the last run took with it names a buffer nothing
        // can read now: delivering it would be a turn spent on stale news.
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

pub fn publish_sessions(d: &Dispatch) {
    if let Ok(rows) = d.sessions.list() {
        d.bus.publish(Event::Sessions {
            sessions: rows.iter().map(|r| session_info(d, r)).collect(),
        });
    }
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
        },
    }
}

/// A stored row as a client is told about it, whatever told it.
///
/// # Invariant
///
/// The tuning fields are what the session *would send now*: its live handle when it
/// has one, else its row. One function, because the greeting and the list must not
/// disagree about a session's model. The level is the one `set_session` stored, so
/// nothing is added here that the request would not carry.
pub fn session_info(d: &Dispatch, row: &crate::store::SessionRow) -> protocol::SessionInfo {
    let tuning = stated_tuning(d.sessions.peek(&row.name).map(|s| s.tuning()), row);
    let entry = tuning
        .model
        .as_deref()
        .and_then(|model| d.providers.place(model));
    // The roster's spelling, else the name the session itself gave. Never the
    // server's own: that is not a model any session runs on.
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
        messages: row.messages,
        cwd: row.cwd.clone(),
        usage: row.usage,
        model,
        thinking_effort,
        context_limit: tuning
            .context_limit
            .or(Some(crate::agent::DEFAULT_CONTEXT_LIMIT_PERCENT)),
    }
}
