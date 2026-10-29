//! The command surface: catalog and dispatch.
//! See `docs/notes/server-commands.md`.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use protocol::{Args, CommandSpec, CommandTarget, CompletionSource, Event, MessageEvent};
use serde_json::json;
use std::time::Duration;

use tokio_util::sync::CancellationToken;

use crate::agent::Agent;
use crate::bus::EventBus;
use crate::session::SessionManager;
use crate::store::{PendingTask, Store, Transcript};

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
    /// The machine's LLM roster: what `set_session --llm` is checked against.
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
    let active: ActiveGuard = ActiveGuard::new(d.active.clone());
    tokio::spawn(async move {
        let _active = active;
        agent
            .run_turn(&work.session, work.tasks, work.epoch, work.gate)
            .await;
    });
}

/// Claims one idle session; an inject-only recovery gets a durable turn row before
/// its inbox is delivered.
fn claim_recovery(
    d: &Dispatch,
    session: crate::session::SessionHandle,
    mut tasks: Vec<i64>,
) -> Option<TurnWork> {
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
pub fn server_catalog() -> Vec<CommandSpec> {
    vec![
        CommandSpec {
            target: CommandTarget::Server,
            name: "send_message".into(),
            summary: "Send a message to a session and run a turn.".into(),
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
            summary: "Create the session if it does not exist, and apply the given settings to \
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
                    "Working directory for a newly created session. Defaults to the server's.",
                    false,
                    Some("path"),
                    // The client lists its own filesystem: it is on the same
                    // machine, and it is the one the user was looking at.
                    CompletionSource::Paths,
                ),
                arg(
                    "llm",
                    "Roster entry to run this session's turns on, as <provider>/<model> (or a \
                     bare model id). Defaults to the model the session was created with.",
                    false,
                    Some("provider/model"),
                    CompletionSource::Static(crate::providers::roster().specs()),
                ),
                arg(
                    "compact_threshold",
                    "Compact the transcript once it fills this much of the model's context, e.g. \
                     `30%`. 0 turns compaction off. Defaults to a share of the \
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
            name: "del_session".into(),
            summary: "Delete a session, its shells and its transcript.".into(),
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
            summary: "Stop the turn one session is running. The turn is dropped, not resumed on \
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
            summary: "Stop running turns, save what is left undone, and shut the server down. \
                      Detached jobs are left running."
                .into(),
            args: vec![],
        },
        CommandSpec {
            target: CommandTarget::Server,
            name: "get_messages".into(),
            summary: "Read the transcript: a range of sequence numbers, oldest first. \
                     Each row arrives as a `message` event before the result."
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
    summary: &str,
    required: bool,
    value_hint: Option<&str>,
    completions: CompletionSource,
) -> protocol::ArgSpec {
    protocol::ArgSpec {
        name: name.into(),
        summary: summary.into(),
        required,
        value_hint: value_hint.map(str::to_string),
        completions,
    }
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
        "interrupt_session" => interrupt_session(d, args).await,
        "cancel" => cancel(d).await,
        "get_messages" => get_messages(d, args).await,
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
        return Err(Outcome::err(format!(
            "{name} is missing required argument(s): {}",
            missing.join(", ")
        )));
    }
    Ok(spec.clone())
}

impl Dispatch {
    /// Every command offered, from all three sources: the only list there is.
    pub async fn catalog(&self) -> Vec<CommandSpec> {
        let mut all = server_catalog();
        all.extend(protocol::client_commands());
        all.extend(self.scripts.commands().await);
        all
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
    let session = match d.sessions.get(&name) {
        Ok(s) => s,
        Err(e) => return Outcome::err(e.to_string()),
    };
    if session.is_deleted() {
        return Outcome::err(format!("session {name:?} is being deleted"));
    }
    let epoch = session.cancel_epoch();
    if let Some(gate) = session.try_claim() {
        // The gate is claimed before the durable write, so a second sender can only
        // inject, never spawn a competing turn.
        match d.store.begin_turn(session.id(), &name, &message) {
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
                Outcome::ok(format!("message accepted by {name}"))
            }
            Err(e) => Outcome::err(format!("could not record the pending turn: {e}")),
        }
    } else {
        match d.store.add_pending_task(
            &name,
            "inject",
            &json!({"session": name, "message": message}),
        ) {
            Ok(_) => {
                session.note_message();
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
    // Resolve `--llm` now: a typo should fail the command that made it.
    let entry = match args.get("llm") {
        None => None,
        Some(spec) => match resolve_llm(d, spec) {
            Ok(entry) => Some(entry),
            Err(e) => return Outcome::err(e),
        },
    };
    let threshold = match args.get("compact_threshold") {
        None => None,
        Some(raw) => match parse_percent(raw) {
            Ok(v) => Some(v),
            Err(e) => return Outcome::err(e),
        },
    };
    // A percentage needs a window; silently keeping the old budget would lie.
    let context = entry
        .as_ref()
        .map(|e| e.context)
        .unwrap_or(d.agent.default_context);
    if let (Some(percent), None) = (threshold, context) {
        return Outcome::err(format!(
            "--compact_threshold {percent}% needs a context window to be a percentage of, and \
             no model involved here has one on record{}",
            match &entry {
                Some(e) => format!(" ({} is not in the roster with a `context`)", e.spec()),
                None => " (set --llm to a roster entry that states one)".to_string(),
            }
        ));
    }
    // Naming a session creates it: that is the only way to get one.
    let session = match d
        .sessions
        .ensure(&name, args.get("cwd").map(String::as_str))
    {
        Ok(session) => session,
        Err(e) => return Outcome::err(e.to_string()),
    };

    let mut tuning = session.tuning();
    let mut changed = false;
    if let Some(level) = args.get("thinking_effort") {
        // Check the level against the model this session will run on, so typos fail now.
        let target = entry.clone().or_else(|| {
            tuning
                .llm
                .as_deref()
                .and_then(|spec| d.providers.entry(spec).ok())
        });
        tuning.thinking_effort = Some(match &target {
            Some(e) => match e.reasoning_effort(level) {
                Ok(level) => level,
                Err(message) => return Outcome::err(message),
            },
            // Not in the roster, so nothing to check against: pass it to the provider.
            None => level.clone(),
        });
        changed = true;
    }
    if let Some(entry) = &entry {
        tuning.llm = Some(entry.spec());
        changed = true;
    }
    if let Some(percent) = threshold {
        tuning.compact_threshold = Some(percent);
        changed = true;
    }
    if changed {
        // Persist before applying: a restart must restore what turns here read.
        if let Err(e) = d.store.set_session_tuning(
            session.id(),
            tuning.llm.as_deref(),
            tuning.compact_threshold,
            tuning.thinking_effort.as_deref(),
            // The model the session will actually run on: the entry's, or the
            // server's own when it has none of its own.
            entry
                .as_ref()
                .map(|e| e.model.as_str())
                .unwrap_or_else(|| d.agent.llm.model()),
        ) {
            return Outcome::err(format!("could not save the session settings: {e}"));
        }
        session.set_tuning(tuning.clone());
    }
    publish_sessions(d);

    let cwd = session.cwd();
    let mut message = format!("session is now {name}");
    let mut notes: Vec<String> = Vec::new();
    if let Some(llm) = &tuning.llm {
        notes.push(format!("llm {llm}"));
    }
    if let Some(level) = &tuning.thinking_effort {
        notes.push(format!("thinking {level}"));
    }
    if let Some(percent) = tuning.compact_threshold {
        let budget = context.map(|c| crate::providers::tokens_for_percent(c, percent));
        notes.push(match (percent, budget) {
            // 0 is how the agent spells "never compact"; saying "0 tokens"
            // would read as the opposite.
            (0, _) => "compaction off".to_string(),
            (percent, Some(tokens)) => format!("compacting at {percent}% ({tokens} tokens)"),
            (percent, None) => format!("compacting at {percent}%"),
        });
    }
    if !notes.is_empty() {
        message.push_str(&format!(" ({})", notes.join(", ")));
    }
    Outcome::ok_with(
        message,
        json!({
            "session": name,
            "cwd": cwd,
            "llm": tuning.llm,
            "compact_threshold": tuning.compact_threshold,
            "thinking_effort": tuning.thinking_effort,
        }),
    )
}

/// Turns `--llm` into a roster entry, trying `provider/model` then a bare id.
fn resolve_llm(d: &Dispatch, spec: &str) -> Result<crate::providers::Entry, String> {
    match d.providers.resolve(spec) {
        Ok(choice) => Ok(choice.entry),
        Err(e) => d.providers.find_model(spec).map(|c| c.entry).ok_or(e),
    }
}

/// `30%`, `30`, or `0` for "never compact"; anything else is refused, not rounded.
fn parse_percent(raw: &str) -> Result<u8, String> {
    let trimmed = raw.trim().trim_end_matches('%').trim();
    let value: u16 = trimmed
        .parse()
        .map_err(|_| format!("--compact_threshold takes a percentage like `30%`, got {raw:?}"))?;
    if value > 100 {
        return Err(format!(
            "--compact_threshold is a percentage of the context window, so it cannot exceed 100%; \
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

/// Reads the transcript back out, so a caller need not reassemble deltas.
///
/// # Invariant
///
/// Read-only: it takes no session gate, because reading while a turn appends is
/// what the TUI's replay does.
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
        .map(|row| {
            Event::Message(MessageEvent {
                session: name.to_string(),
                seq: row.seq,
                kind: row.kind.clone(),
                role: row.role.clone(),
                name: row.name.clone(),
                text: row.text.clone(),
            })
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
        if let Some(work) = claim_recovery(d, session, owned) {
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
        let owned = tasks
            .iter()
            .filter(|task| task.kind == "turn")
            .map(|task| task.id)
            .collect();
        if let Some(work) = claim_recovery(d, session, owned) {
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
            sessions: rows
                .iter()
                .map(|r| protocol::SessionSummary {
                    name: r.name.clone(),
                    status: r.status,
                    created_at: r.created_at,
                    updated_at: r.updated_at,
                    messages: r.messages,
                    cwd: r.cwd.clone(),
                })
                .collect(),
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recovery_has_one_owner_per_session() {
        let task = |id, session: &str, kind: &str| PendingTask {
            id,
            session: session.into(),
            kind: kind.into(),
            payload: json!({}),
            created_at: 0,
        };
        let groups = recovery_groups(vec![
            task(1, "main", "turn"),
            task(2, "main", "inject"),
            task(3, "main", "inject"),
            task(4, "other", "turn"),
        ]);
        assert_eq!(groups.len(), 2);
        assert_eq!(groups["main"].len(), 3);
        assert_eq!(groups["other"].len(), 1);
    }

    #[test]
    fn finishing_and_preserving_a_turn_use_the_durable_boundary() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(&dir.path().join("g.db")).unwrap();
        let id = store.create_session("main", "m", "C:/x").unwrap();
        let task = store.begin_turn(id, "main", "go").unwrap();

        store
            .finish_turn(id, "main", &[task], protocol::SessionStatus::Idle, true)
            .unwrap();
        assert!(store.pending_tasks().unwrap().iter().any(|t| t.id == task));

        store
            .finish_turn(id, "main", &[task], protocol::SessionStatus::Idle, false)
            .unwrap();
        assert!(store.pending_tasks().unwrap().is_empty());
    }

    /// A panicking turn must still release its slot, or shutdown waits forever.
    #[test]
    fn a_panicking_turn_still_releases_its_slot() {
        let active = Arc::new(AtomicUsize::new(0));
        let counted = active.clone();
        let died = std::thread::spawn(move || {
            let _slot = ActiveGuard::new(counted);
            panic!("a turn blowing up");
        })
        .join();
        assert!(died.is_err(), "the turn did panic");
        assert_eq!(
            active.load(Ordering::SeqCst),
            0,
            "shutdown would never see the count drain"
        );
    }

    /// The server's own commands, and nothing else.
    #[test]
    fn server_catalog_is_exactly_the_known_commands() {
        let catalog = server_catalog();
        let mut names: Vec<&str> = catalog.iter().map(|c| c.name.as_str()).collect();
        names.sort();
        assert_eq!(
            names,
            vec![
                "cancel",
                "del_session",
                "get_messages",
                "interrupt_session",
                "send_message",
                "set_session"
            ]
        );
        assert!(
            catalog.iter().all(|c| c.target == CommandTarget::Server),
            "every entry here runs in this process"
        );
    }

    /// The client's commands are published too, and say that they run there.
    #[test]
    fn the_client_commands_are_published_as_client_ones() {
        let mut names: Vec<String> = protocol::client_commands()
            .iter()
            .map(|c| c.name.clone())
            .collect();
        names.sort();
        assert_eq!(
            names,
            vec![
                "del_event_filter",
                "del_sticky_cmd_argument",
                "set_event_filter",
                "set_event_split",
                "set_event_ui",
                "set_sticky_cmd_argument"
            ]
        );
        assert!(
            protocol::client_commands()
                .iter()
                .all(|c| c.target == CommandTarget::Client),
            "the server must not claim to run these"
        );
        // The names the client owns stay on the client: the server names the
        // source and nothing more.
        let sticky = protocol::client_commands()
            .into_iter()
            .find(|c| c.name == "del_sticky_cmd_argument")
            .unwrap();
        assert!(matches!(
            sticky.args[0].completions,
            CompletionSource::StickyArguments
        ));
    }

    /// The whole published catalog, as a greeting would carry it.
    fn published() -> Vec<CommandSpec> {
        let mut all = server_catalog();
        all.extend(protocol::client_commands());
        all
    }

    /// A client command sent over the wire is refused by name, not called unknown.
    #[test]
    fn a_client_command_is_refused_with_a_reason() {
        let out = resolve(
            &published(),
            "set_event_ui",
            &Args::from([("block_size".to_string(), "3".to_string())]),
        )
        .unwrap_err();
        assert!(!out.ok);
        assert!(
            out.message.contains("runs in the client"),
            "the refusal must say where it runs: {}",
            out.message
        );
    }

    /// One resolve for a built-in, a script command, and a name nobody knows.
    #[test]
    fn resolve_tells_the_three_kinds_apart() {
        let mut all = published();
        all.push(CommandSpec {
            name: "weather".into(),
            summary: "Registered by a script.".into(),
            args: vec![arg(
                "city",
                "Which city.",
                true,
                None,
                CompletionSource::None,
            )],
            target: CommandTarget::ServerScript,
        });

        let builtin = resolve(
            &all,
            "cancel",
            &Args::from([("session".to_string(), "x".to_string())]),
        )
        .unwrap();
        assert_eq!(builtin.target, CommandTarget::Server);

        let scripted = resolve(
            &all,
            "weather",
            &Args::from([("city".to_string(), "Oslo".to_string())]),
        )
        .unwrap();
        assert_eq!(scripted.target, CommandTarget::ServerScript);

        // Missing arguments are judged the same way for both.
        let missing = resolve(&all, "weather", &Args::default()).unwrap_err();
        assert!(missing.message.contains("city"), "{}", missing.message);

        let unknown = resolve(&all, "weather2", &Args::default()).unwrap_err();
        assert_eq!(unknown.message, "unknown command \"weather2\"");
    }

    #[test]
    fn session_args_are_completable() {
        let spec = server_catalog()
            .into_iter()
            .find(|c| c.name == "send_message")
            .unwrap();
        let session = spec.args.iter().find(|a| a.name == "session").unwrap();
        assert!(matches!(session.completions, CompletionSource::Sessions));
        // There is no implicit "current session" to fall back on, so naming it
        // is not optional.
        assert!(session.required, "--session is required");
    }

    #[test]
    fn cancel_takes_no_arguments() {
        let spec = server_catalog()
            .into_iter()
            .find(|c| c.name == "cancel")
            .unwrap();
        assert!(spec.args.is_empty(), "cancel is a whole-server stop");
    }

    /// Interrupting names one session; guessing one is the hidden state we avoid.
    #[test]
    fn interrupt_session_names_one_session() {
        let spec = server_catalog()
            .into_iter()
            .find(|c| c.name == "interrupt_session")
            .unwrap();
        let name = spec.args.iter().find(|a| a.name == "session").unwrap();
        assert!(name.required, "--session is required");
        assert!(matches!(name.completions, CompletionSource::Sessions));
    }

    /// Every command spells its session the same way, so sticky `session` works.
    #[test]
    fn every_command_takes_a_session_or_a_message() {
        for spec in server_catalog() {
            // `cancel` is the one command with nothing to configure.
            if spec.name == "cancel" {
                continue;
            }
            let required: Vec<&str> = spec
                .args
                .iter()
                .filter(|a| a.required)
                .map(|a| a.name.as_str())
                .collect();
            assert!(
                required.contains(&"message") || required.contains(&"session"),
                "{} requires one of message/session",
                spec.name
            );
            assert!(
                !spec.args.iter().any(|a| a.name == "name"),
                "{} still spells the session as --name",
                spec.name
            );
        }
    }

    /// Every completion source must survive the wire, or Tab silently offers nothing.
    #[test]
    fn completion_sources_survive_the_wire() {
        let json = serde_json::to_string(&server_catalog()).unwrap();
        let back: Vec<CommandSpec> = serde_json::from_str(&json).unwrap();
        let source = |command: &str, arg: &str| {
            back.iter()
                .find(|c| c.name == command)
                .and_then(|c| c.args.iter().find(|a| a.name == arg))
                .map(|a| a.completions.clone())
                .unwrap_or(CompletionSource::None)
        };
        assert!(matches!(
            source("send_message", "session"),
            CompletionSource::Sessions
        ));
        assert!(matches!(
            source("set_session", "cwd"),
            CompletionSource::Paths
        ));
        assert!(matches!(
            source("set_session", "compact_threshold"),
            CompletionSource::Static(_)
        ));
        // Free-form text stays free-form: the catalogue says nothing, and the
        // client must not invent a list for it.
        assert!(matches!(
            source("send_message", "message"),
            CompletionSource::None
        ));
    }

    /// `set_session` chooses model and compaction, so both flags must complete.
    #[test]
    fn set_session_offers_the_roster_and_a_percentage() {
        let spec = server_catalog()
            .into_iter()
            .find(|c| c.name == "set_session")
            .unwrap();
        let session = spec.args.iter().find(|a| a.name == "session").unwrap();
        assert!(session.required);
        assert!(matches!(session.completions, CompletionSource::Sessions));
        let llm = spec.args.iter().find(|a| a.name == "llm").unwrap();
        assert!(!llm.required, "omitting --llm keeps the server's model");
        assert!(matches!(llm.completions, CompletionSource::Static(_)));
        let threshold = spec
            .args
            .iter()
            .find(|a| a.name == "compact_threshold")
            .unwrap();
        assert!(!threshold.required);
        // The valid range is wide, but the points worth tabbing to are few.
        match &threshold.completions {
            CompletionSource::Static(values) => {
                assert!(values.iter().any(|v| v == "0"), "{values:?}");
                assert!(values.iter().any(|v| v == "50%"), "{values:?}");
            }
            other => panic!("--compact_threshold should offer values, got {other:?}"),
        }
        // `--cwd` is the one value the catalog cannot list itself: it has to ask
        // the filesystem the client is sitting on.
        let cwd = spec.args.iter().find(|a| a.name == "cwd").unwrap();
        assert!(matches!(cwd.completions, CompletionSource::Paths));
    }

    #[test]
    fn a_percentage_is_parsed_or_refused() {
        assert_eq!(parse_percent("30%"), Ok(30));
        assert_eq!(parse_percent(" 30 % "), Ok(30));
        assert_eq!(parse_percent("0"), Ok(0));
        assert_eq!(parse_percent("100%"), Ok(100));
        assert!(parse_percent("101%").is_err());
        assert!(parse_percent("half").is_err());
        assert!(parse_percent("-5").is_err());
    }

    /// The slice syntax, case by case: this is the whole grammar.
    #[test]
    fn a_seq_slice_is_parsed_like_a_python_slice() {
        // One index names one row; -1 is the newest, like `rows[-1]`.
        assert_eq!(
            parse_seq_slice("5", 10),
            Ok(SeqWindow { first: 5, last: 5 })
        );
        assert_eq!(
            parse_seq_slice("-1", 10),
            Ok(SeqWindow {
                first: 10,
                last: 10
            })
        );
        // `-n:` is the newest n.
        assert_eq!(
            parse_seq_slice("-5:", 10),
            Ok(SeqWindow { first: 6, last: 10 })
        );
        // Both ends inclusive, which is the documented difference from Python.
        assert_eq!(
            parse_seq_slice("5:10", 10),
            Ok(SeqWindow { first: 5, last: 10 })
        );
        assert_eq!(
            parse_seq_slice("1:1", 10),
            Ok(SeqWindow { first: 1, last: 1 })
        );
        // An omitted bound takes the end it stands for.
        assert_eq!(
            parse_seq_slice(":10", 10),
            Ok(SeqWindow { first: 1, last: 10 })
        );
        assert_eq!(
            parse_seq_slice("5:", 10),
            Ok(SeqWindow { first: 5, last: 10 })
        );
        // Two negatives are a window inside the tail.
        assert_eq!(
            parse_seq_slice("-5:-2", 10),
            Ok(SeqWindow { first: 6, last: 9 })
        );
        // Whitespace is the shell's doing, not the caller's mistake.
        assert_eq!(
            parse_seq_slice(" 5 : 10 ", 10),
            Ok(SeqWindow { first: 5, last: 10 })
        );
    }

    /// Asking for more than exists is fewer rows, not an error.
    #[test]
    fn a_seq_slice_clamps_to_the_transcript() {
        // Past the tail.
        assert_eq!(
            parse_seq_slice("5:99", 10),
            Ok(SeqWindow { first: 5, last: 10 })
        );
        // Further back than the transcript goes.
        assert_eq!(
            parse_seq_slice("-99:", 10),
            Ok(SeqWindow { first: 1, last: 10 })
        );
        // Past the end entirely: an empty window, and the reply still says the tail.
        let w = parse_seq_slice("99:", 10).unwrap();
        assert!(w.first > w.last, "{w:?}");
        // An empty transcript is empty, not a panic.
        assert!(parse_seq_slice("-1", 0).unwrap().first > 0);
        assert!(parse_seq_slice(":5", 0).unwrap().first > 0);
    }

    /// Each of these is a typo, and each says what it wanted.
    #[test]
    fn a_bad_seq_slice_is_refused() {
        assert!(parse_seq_slice("", 10).is_err());
        assert!(parse_seq_slice(":", 10).is_err());
        assert!(parse_seq_slice("0", 10).is_err(), "seq starts at 1");
        assert!(parse_seq_slice("0:", 10).is_err());
        assert!(parse_seq_slice(":-0", 10).is_err());
        assert!(parse_seq_slice("a:b", 10).is_err());
        assert!(parse_seq_slice("1:2:3", 10).is_err(), "no step");
        assert!(parse_seq_slice("10:5", 10).is_err(), "written backwards");
        assert!(
            parse_seq_slice("-2:-5", 10).is_err(),
            "backwards, tail-wise"
        );
        assert!(
            parse_seq_slice(&format!("-{}", i64::MAX), 10)
                .unwrap()
                .first
                >= 1,
            "a huge negative clamps instead of overflowing"
        );
    }

    /// One way to name rows, not two: a cursor and a range would be rivals.
    #[test]
    fn get_messages_names_rows_one_way() {
        let spec = server_catalog()
            .into_iter()
            .find(|c| c.name == "get_messages")
            .unwrap();
        let session = spec.args.iter().find(|a| a.name == "session").unwrap();
        assert!(session.required);
        assert!(matches!(session.completions, CompletionSource::Sessions));

        let range = spec.args.iter().find(|a| a.name == "range").unwrap();
        assert!(
            range.required,
            "SPEC names the range, so it is not optional"
        );
        // The cursor form is gone: it was this range plus a truncation policy.
        assert!(
            !spec.args.iter().any(|a| a.name == "after_seq"),
            "the cursor spelling is gone"
        );
        assert!(
            !spec.args.iter().any(|a| a.name == "limit"),
            "a row cap is the caller's business, not this command's"
        );
    }

    /// The reply is the rows as events, plus `last_seq` and the session's `status`.
    ///
    /// # Invariant
    ///
    /// Asserted on the reply builder, not through a live dispatch: what matters
    /// here are fields, not I/O.
    #[test]
    fn get_messages_reports_the_session_status_with_the_rows() {
        let rows = vec![
            crate::store::MessageRow::parse(1, "text", r#"{"role":"user","text":"hi"}"#),
            crate::store::MessageRow::parse(2, "text", r#"{"role":"assistant","text":"done"}"#),
        ];
        for (status, spelled) in [
            (protocol::SessionStatus::Running, "running"),
            (protocol::SessionStatus::Idle, "idle"),
        ] {
            let out = transcript_outcome(
                "main",
                Transcript {
                    last_seq: 2,
                    status,
                    messages: rows.clone(),
                },
            );
            assert!(out.ok);
            let data = out
                .data
                .expect("a reader needs the data, not just a sentence");
            // Visible with `--nocapture`: the reply shape, verbatim.
            println!("get_messages reply: {data}");
            assert_eq!(
                data["status"], spelled,
                "status is spelled in snake_case on the wire: {data}"
            );
            assert_eq!(data["last_seq"], 2);
            assert_eq!(data["session"], "main");
            // The rows are not in the result: each one is its own event, sent first.
            assert!(
                data.get("messages").is_none(),
                "a read that is also a blob has two answers: {data}"
            );
            let rows: Vec<&str> = out
                .events
                .iter()
                .map(|event| match event {
                    Event::Message(row) => row.text.as_str(),
                    other => panic!("a read answers with rows, not {other:?}"),
                })
                .collect();
            assert_eq!(rows, vec!["hi", "done"], "{rows:?}");
            let seqs: Vec<i64> = out
                .events
                .iter()
                .map(|event| match event {
                    Event::Message(row) => row.seq,
                    other => panic!("{other:?}"),
                })
                .collect();
            assert_eq!(
                seqs,
                vec![1, 2],
                "oldest first, each saying which row it is"
            );
        }

        // An empty window still says what the session is doing: that is the state a
        // poller is waiting to change.
        let out = transcript_outcome(
            "main",
            Transcript {
                last_seq: 7,
                status: protocol::SessionStatus::Running,
                messages: vec![],
            },
        );
        let data = out.data.unwrap();
        assert_eq!(data["status"], "running");
        assert!(out.events.is_empty(), "no rows, no events");
        assert!(
            out.message.contains("transcript ends at seq 7"),
            "the sentence still says where the transcript ends: {}",
            out.message
        );
    }

    /// A read creates no session.
    ///
    /// # Invariant
    ///
    /// A name nobody created is still refused -- `set_session` is the only thing
    /// that makes a session (`ensure`), and a read that quietly materialised one
    /// would let a poller fill the database with ghosts.
    #[test]
    fn get_messages_still_refuses_a_session_nobody_created() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(&dir.path().join("g.db")).unwrap();
        let config = crate::config::BashConfig {
            program: None,
            timeout: Duration::from_secs(1),
            max_output_chars: 0,
        };
        let detached = crate::tools::detached::DetachedRegistry::new(
            store.clone(),
            None,
            dir.path().join("detached"),
            vec![],
        );
        let sessions = SessionManager::new(store.clone(), config, "m".into(), detached, vec![]);

        let err = match sessions.get("ghost") {
            Ok(_) => panic!("a name nobody created must not resolve"),
            Err(e) => e.to_string(),
        };
        assert_eq!(
            err, "no session named \"ghost\"",
            "the refusal is the same one get_messages returns"
        );
        assert!(
            store.list_sessions().unwrap().is_empty(),
            "reading must not create: {err}"
        );

        // An existing session is read without being created again. No
        // `sessions.get`: that would need a shell.
        let id = store.create_session("real", "m", "C:/x").unwrap();
        assert_eq!(store.list_sessions().unwrap().len(), 1);
        let t = store.messages_between(id, 1, i64::MAX).unwrap();
        assert_eq!(t.last_seq, 0);
        assert_eq!(t.status, protocol::SessionStatus::Idle);
        assert_eq!(
            store.list_sessions().unwrap().len(),
            1,
            "still just the one row"
        );
    }
}
