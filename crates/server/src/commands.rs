//! The command surface: catalog and dispatch.
//!
//! The server answers five commands: `send_message`, `set_session`,
//! `del_session`, `interrupt_session` and `cancel`. Stopping work is a command
//! and only a command: there is no side-channel that cancels a turn, so a
//! script and a keystroke reach the same code by the same door. Sticky
//! arguments are a client-side concern and never appear here.
//!
//! There is no "current session". A command either names its session or fails;
//! the only way to get a default is the client's `persistant_cmd_argument`,
//! which is filled in before the command ever reaches the wire.
//!
//! The catalog is published to clients at connect time, which is what lets the
//! CLI's completer stay in sync without a second source of truth.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use protocol::{Args, CommandSpec, CompletionSource, Event};
use serde_json::json;
use std::time::Duration;

use tokio_util::sync::CancellationToken;

use crate::agent::Agent;
use crate::bus::EventBus;
use crate::session::SessionManager;
use crate::store::Store;

/// How long `cancel` waits before pulling the plug, so the acknowledgement
/// still reaches whoever asked for it.
const SHUTDOWN_NOTICE: Duration = Duration::from_millis(250);

/// Default window for `get_messages`. Enough to hold a whole turn (a turn with
/// 20 tool calls is ~45 rows) without making a caller read the entire history.
const DEFAULT_MESSAGE_LIMIT: usize = 50;
/// Cap, so a typo cannot pull megabytes of transcript through a UDP channel
/// that fragments at 1174 bytes a packet.
const MAX_MESSAGE_LIMIT: usize = 500;

pub struct Dispatch {
    pub agent: Arc<Agent>,
    pub sessions: SessionManager,
    pub store: Store,
    pub bus: EventBus,
    /// The machine's LLM roster, which is what `set_session --llm` is checked
    /// against and where its completion list comes from.
    pub providers: &'static crate::providers::Catalog,
    /// Turns currently running. `cancel` waits for this to drain before the
    /// process is allowed to exit.
    pub active: Arc<AtomicUsize>,
    /// Pulled by `cancel`; the listen loop stops accepting work when it fires.
    pub shutdown: CancellationToken,
}

/// The result of one command, sent back as a `cmd_result` event.
pub struct Outcome {
    pub ok: bool,
    pub message: String,
    pub data: Option<serde_json::Value>,
}

impl Outcome {
    fn ok(message: impl Into<String>) -> Self {
        Self { ok: true, message: message.into(), data: None }
    }

    fn ok_with(message: impl Into<String>, data: serde_json::Value) -> Self {
        Self { ok: true, message: message.into(), data: Some(data) }
    }

    fn err(message: impl Into<String>) -> Self {
        Self { ok: false, message: message.into(), data: None }
    }
}

pub fn catalog() -> Vec<CommandSpec> {
    vec![
        CommandSpec {
            name: "send_message".into(),
            summary: "Send a message to a session and run a turn.".into(),
            args: vec![
                arg("message", "The message text.", true, Some("text"), CompletionSource::None),
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
                    CompletionSource::Static(
                        crate::providers::roster().specs(),
                    ),
                ),
                arg(
                    "compact_threshold",
                    "Compact the transcript once it fills this much of the model's context, e.g. \
                     `30%`. 0 turns compaction off. Defaults to the server's character budget.",
                    false,
                    Some("n%"),
                    // Any 0-100 is accepted; the list is the points worth
                    // offering, not the ones allowed.
                    CompletionSource::Static(
                        ["0", "10%", "20%", "30%", "40%", "50%", "60%", "70%", "80%", "90%"]
                            .iter()
                            .map(|s| s.to_string())
                            .collect(),
                    ),
                ),
            ],
        },
        CommandSpec {
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
            name: "cancel".into(),
            summary: "Stop running turns, save what is left undone, and shut the server down. \
                      Detached jobs are left running."
                .into(),
            args: vec![],
        },
        CommandSpec {
            name: "get_messages".into(),
            summary: "Read the transcript: rows after a sequence number, oldest first.".into(),
            args: vec![
                arg(
                    "session",
                    "Session to read.",
                    true,
                    Some("name"),
                    CompletionSource::Sessions,
                ),
                arg(
                    "after_seq",
                    "Only rows with a sequence number greater than this. Defaults to 0 (from the \
                     start, subject to --limit).",
                    false,
                    Some("n"),
                    CompletionSource::None,
                ),
                arg(
                    "limit",
                    "How many rows to return, counted back from the end. Defaults to 50.",
                    false,
                    Some("n"),
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
    let missing = catalog()
        .into_iter()
        .find(|c| c.name == name)
        .map(|c| {
            c.args
                .iter()
                .filter(|a| a.required && !args.contains_key(&a.name))
                .map(|a| a.name.clone())
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    if !missing.is_empty() {
        return Outcome::err(format!(
            "{name} is missing required argument(s): {}",
            missing.join(", ")
        ));
    }

    match name {
        "send_message" => send_message(d, args).await,
        "set_session" => set_session(d, args).await,
        "del_session" => del_session(d, args).await,
        "interrupt_session" => interrupt_session(d, args).await,
        "cancel" => cancel(d).await,
        "get_messages" => get_messages(d, args).await,
        other => Outcome::err(format!("unknown command {other:?}")),
    }
}

async fn send_message(d: &Dispatch, args: &Args) -> Outcome {
    let Some(message) = args.get("message").cloned() else {
        return Outcome::err("send_message requires --message");
    };
    // No implicit current session: the name is either here or the command is
    // wrong. A default is the client's business (a sticky `session`), not the
    // server's hidden state.
    let Some(name) = args.get("session").cloned() else {
        return Outcome::err(
            "send_message requires --session; set a sticky `session` to avoid typing it",
        );
    };
    let session = match d.sessions.get(&name) {
        Ok(s) => s,
        Err(e) => return Outcome::err(e.to_string()),
    };
    let label = session.name().to_string();
    // A message that arrives while this session is working does not start a
    // second turn: it goes into the inbox and the running turn folds it in at
    // its next step boundary. Starting a turn would have meant queueing behind
    // the gate -- the operator's line unread until the agent had finished
    // everything else and stopped, which is exactly when it is too late to
    // steer.
    //
    // `kind` records which contract the row has. "turn" means the message is
    // already in the transcript and the row only proves a turn is owed;
    // "inject" means the message has not been written yet and whoever consumes
    // it must write it. A restart has to tell them apart.
    let busy = session.busy();
    let kind = if busy { "inject" } else { "turn" };
    // Remember which row is this message's. Clearing by session used to delete
    // the rows of messages still waiting their turn: the first turn finished
    // and took the second one's pending row with it, so a crash before the
    // second turn ran lost the message entirely -- it was neither in the
    // transcript nor in the pending list.
    let task_id = match d
        .store
        .add_pending_task(&label, kind, &json!({"session": label, "message": message}))
    {
        Ok(id) => id,
        Err(e) => return Outcome::err(format!("could not record the pending turn: {e}")),
    };

    // Checked after the write, not before: if the turn ended in between, the
    // row is already there and this will start the turn that consumes it.
    if session.busy() {
        // No turn will be started for this row, so its contract changes: the
        // message is not in the transcript and must be written by whoever
        // consumes it, including a restart.
        let _ = d.store.set_pending_kind(task_id, "inject");
        return Outcome::ok(format!(
            "message queued for {label}; it will be read at the next step"
        ));
    }

    let agent = d.agent.clone();
    let store = d.store.clone();
    let active = d.active.clone();
    let session_resumable = session.clone();
    active.fetch_add(1, Ordering::SeqCst);
    tokio::spawn(async move {
        agent.run_turn(&session, &message, Some(task_id)).await;
        // The turn is done, so it is no longer something the next start owes --
        // unless a `cancel` interrupted it, in which case staying pending is
        // exactly what "save what is left undone" promised.
        if !session_resumable.take_resumable() {
            let _ = store.delete_pending_task(task_id);
        }
        active.fetch_sub(1, Ordering::SeqCst);
    });
    Outcome::ok(format!("message accepted by {label}"))
}

async fn set_session(d: &Dispatch, args: &Args) -> Outcome {
    let name = args["session"].clone();
    // `--llm` is resolved here rather than at the next turn: a typo should fail
    // the command that made it, and the roster can answer with what would have
    // worked instead of leaving a turn to die later.
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
    // A percentage is a percentage of something. If the model has no context
    // window on record, the number has no meaning -- and quietly keeping the
    // old budget would look like the setting was applied.
    let context = entry.as_ref().map(|e| e.context).unwrap_or(d.agent.default_context);
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
    // `set_session` doubles as creation: naming a session is the only way to
    // get one, and refusing to create it would make the command useless.
    let session = match d.sessions.ensure(&name, args.get("cwd").map(String::as_str)) {
        Ok(session) => session,
        Err(e) => return Outcome::err(e.to_string()),
    };

    let mut tuning = session.tuning();
    let mut changed = false;
    if let Some(entry) = &entry {
        tuning.llm = Some(entry.spec());
        changed = true;
    }
    if let Some(percent) = threshold {
        tuning.compact_threshold = Some(percent);
        changed = true;
    }
    if changed {
        // Written down before it is applied: a restart has to bring the session
        // back the way it was left, and the in-memory copy is only what turns
        // read.
        if let Err(e) = d.store.set_session_tuning(
            session.id(),
            tuning.llm.as_deref(),
            tuning.compact_threshold,
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

    let cwd = session.cwd().await;
    let mut message = format!("session is now {name}");
    let mut notes: Vec<String> = Vec::new();
    if let Some(llm) = &tuning.llm {
        notes.push(format!("llm {llm}"));
    }
    if let Some(percent) = tuning.compact_threshold {
        let budget = context.map(|c| crate::providers::chars_for_percent(c, percent));
        notes.push(match (percent, budget) {
            // 0 is how the agent spells "never compact"; saying "0 chars"
            // would read as the opposite.
            (0, _) => "compaction off".to_string(),
            (percent, Some(chars)) => format!("compacting at {percent}% ({chars} chars)"),
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
        }),
    )
}

/// Turns `--llm` into a roster entry.
///
/// `provider/model` first, then a bare model id: model ids may themselves
/// contain a slash, so the fallback is what makes `--llm deepseek-flash` work
/// without making `--llm goat/deepseek/...` ambiguous.
fn resolve_llm(d: &Dispatch, spec: &str) -> Result<crate::providers::Entry, String> {
    match d.providers.resolve(spec) {
        Ok(choice) => Ok(choice.entry),
        Err(e) => d.providers.find_model(spec).map(|c| c.entry).ok_or(e),
    }
}

/// `30%`, `30`, or `0` for "never compact".
///
/// Anything else is a typo, and a typo here would silently change how much of
/// the conversation survives, so it is refused rather than rounded.
fn parse_percent(raw: &str) -> Result<u8, String> {
    let trimmed = raw.trim().trim_end_matches('%').trim();
    let value: u16 = trimmed.parse().map_err(|_| {
        format!("--compact_threshold takes a percentage like `30%`, got {raw:?}")
    })?;
    if value > 100 {
        return Err(format!(
            "--compact_threshold is a percentage of the context window, so it cannot exceed 100%; \
             got {raw}"
        ));
    }
    Ok(value as u8)
}

/// Reads the transcript back out.
///
/// `send_message` is fire-and-forget and streaming events are a delta soup:
/// neither is a good way for a caller that was not watching to learn "what did
/// it finally say". An IM bridge has exactly that problem -- it must reply with
/// one message body, minutes after it handed the turn over. So the transcript
/// itself is queryable, and the bridge reads the answer it is about to relay
/// rather than reassembling it from `response` deltas (which interleave with
/// tool calls and would need the same parsing rules as the UI).
///
/// Read-only and cheap: one indexed range scan. It does not claim the session
/// gate, because reading a transcript while a turn appends to it is exactly
/// what the TUI's replay does, and neither has anything to write.
async fn get_messages(d: &Dispatch, args: &Args) -> Outcome {
    let name = args["session"].clone();
    let session = match d.sessions.get(&name) {
        Ok(s) => s,
        Err(e) => return Outcome::err(e.to_string()),
    };
    let after_seq = match args.get("after_seq") {
        None => 0,
        Some(raw) => match raw.trim().parse::<i64>() {
            Ok(v) if v >= 0 => v,
            _ => {
                return Outcome::err(format!(
                    "--after_seq must be a non-negative number, got {raw:?}"
                ));
            }
        },
    };
    let limit = match args.get("limit") {
        None => DEFAULT_MESSAGE_LIMIT,
        Some(raw) => match raw.trim().parse::<usize>() {
            Ok(0) => return Outcome::err("--limit must be at least 1"),
            Ok(v) => v.min(MAX_MESSAGE_LIMIT),
            Err(_) => return Outcome::err(format!("--limit must be a number, got {raw:?}")),
        },
    };
    match d.store.messages_after(session.id(), after_seq, limit) {
        Ok(t) => Outcome::ok_with(
            format!(
                "{} message(s) after seq {after_seq} in {name} (transcript ends at seq {})",
                t.messages.len(),
                t.last_seq
            ),
            json!({
                "session": name,
                "last_seq": t.last_seq,
                "messages": t.messages,
            }),
        ),
        Err(e) => Outcome::err(e.to_string()),
    }
}

async fn del_session(d: &Dispatch, args: &Args) -> Outcome {
    let name = args["session"].clone();
    // Delete means "not this session and not what it is doing", so stop the
    // turn first. Queueing behind it would make the delete wait for the turn
    // to finish on its own, which is not what anyone asking for a delete
    // means -- and it is why this does not simply `claim` and wait.
    let (was_busy, dropped) = interrupt(d, &name);
    // The turn still has to let go of the transcript before the rows go, so
    // the gate is taken anyway. After an interrupt it is a wait measured in
    // milliseconds; without the interrupt it is the length of the turn.
    let _gate = match d.sessions.peek(&name) {
        Some(session) => Some(session.claim().await),
        None => None,
    };
    match d.sessions.delete(&name) {
        Ok(true) => {
            // Deleting a session is the one thing that means "the work it
            // started is not wanted either". `cancel` disagrees on purpose:
            // detached jobs exist to outlive the server's own shutdown.
            let stopped = d.sessions.detached().stop_session(&name).await;
            publish_sessions(d);
            let mut message = if stopped.total() == 0 {
                format!("deleted session {name}")
            } else {
                format!("deleted session {name} and stopped {} detached job(s)", stopped.stopped)
            };
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

/// Stop one session's turn, and drop what it still owed.
///
/// The counterpart to `cancel`, which stops everything but keeps the work: this
/// one is about a single session and it throws the work away. The difference is
/// a single pending row, and the distinction is worth a command of its own --
/// "stop that, I did not mean it" and "stop, I will continue after a restart"
/// are not the same request.
///
/// It used to be Ctrl+X, which meant only the TUI could do it: a script, and so
/// the model, had no way to stop a turn it had started. As a command it is
/// reachable by everyone who can reach the server.
/// Stops what a session is doing and throws the work away.
///
/// Shared by `interrupt_session` and `del_session`. Deleting a session means
/// not wanting what it is doing either, and queueing politely behind a turn
/// would make the delete wait for however long that turn takes -- twenty
/// minutes, if that is how long the turn takes. So delete interrupts first and
/// only then waits, which is a wait measured in milliseconds.
fn interrupt(d: &Dispatch, name: &str) -> (bool, usize) {
    let Some(session) = d.sessions.peek(name) else {
        return (false, 0);
    };
    // Read before the cancel: afterwards there is nothing left to ask.
    let was_busy = session.busy();
    session.cancel();
    // Not `cancel_and_preserve`. Aborting is deliberate, so this work is not
    // what the next start owes -- and clearing the rows is what makes that
    // true for the turns queued behind the running one as well, since they
    // give up once they reach the front.
    let dropped = d.store.clear_pending_tasks(name).unwrap_or(0);
    (was_busy, dropped)
}

async fn interrupt_session(d: &Dispatch, args: &Args) -> Outcome {
    let name = args["session"].clone();
    // `peek`, not `get`: a session with no handle in memory is not running
    // anything, and materialising it would start its shells just to stop
    // nothing. Both "idle" and "no such session" are still worth saying out
    // loud -- a silent success would read as "the turn did stop".
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

/// The SPEC's `cancle`: do not die on the spot. Running turns are cancelled,
/// whatever they cannot serialise into the database is left as a pending task,
/// and only then does the process leave.
///
/// Detached jobs are the exception and the point of the whole exercise: they
/// were started precisely so that they would survive this. Killing them here
/// would make `detached` indistinguishable from `background`, and would take
/// away the one thing an agent needs in order to replace the server it is
/// running on.
async fn cancel(d: &Dispatch) -> Outcome {
    let running = d.sessions.live_sessions();
    for session in &running {
        // Not `cancel()`: a cancelled turn must still be there for the next
        // start to pick up, otherwise "cancel, restart, continue" -- the whole
        // point of shutting down cleanly -- loses the turn it was protecting.
        session.cancel_and_preserve();
    }
    let detached = d.sessions.detached().count().await;
    let pending = d.store.pending_tasks().map(|t| t.len()).unwrap_or(0);
    // Not quite yet: the acknowledgement announcing what is being cancelled is
    // worth more than a fast exit, and the listen loop publishes it only after
    // this handler returns. A quarter second is well inside one round trip.
    let shutdown = d.shutdown.clone();
    tokio::spawn(async move {
        tokio::time::sleep(SHUTDOWN_NOTICE).await;
        shutdown.cancel();
    });
    Outcome::ok(format!(
        "cancelling {} session(s); {} pending task(s) will be picked up on the next start; {} \
         detached job(s) keep running -- they outlive the server, stop them with `kill_bash`",
        running.len(),
        pending,
        detached
    ))
}

/// Wakes sessions that are holding a message nobody is left to read.
///
/// The normal path needs no help: a message written while a turn is running is
/// folded into that turn at its next step boundary. This covers the edge --
/// the turn ended in the same instant the message was written, or a background
/// job finished while the session was idle -- where the row would otherwise sit
/// until the next `send_message`. A mailbox nobody checks is not delivery.
///
/// Only "inject" rows are woken: a "turn" row already belongs to a turn that is
/// starting or queued, and starting a second one for it would ask the model to
/// continue for no reason.
pub fn nudge_inboxes(d: &Dispatch) -> usize {
    let Ok(tasks) = d.store.pending_tasks() else {
        return 0;
    };
    let mut seen = std::collections::HashSet::new();
    let mut started = 0;
    for task in tasks {
        if task.kind != "inject" || !seen.insert(task.session.clone()) {
            continue;
        }
        let Ok(session) = d.sessions.get(&task.session) else {
            continue;
        };
        if session.busy() {
            continue;
        }
        let agent = d.agent.clone();
        let active = d.active.clone();
        active.fetch_add(1, Ordering::SeqCst);
        // `None` as the owning row: this turn was not started from a message,
        // so the step-boundary drain may take every queued row it finds.
        tokio::spawn(async move {
            agent.resume_turn(&session, None).await;
            active.fetch_sub(1, Ordering::SeqCst);
        });
        started += 1;
    }
    started
}

/// Turns that were interrupted, replayed once the server is back.
///
/// A turn is replayed by asking the model to continue: its transcript is
/// already durable, so this resumes rather than restarts.
pub async fn resume_pending(d: &Dispatch) -> usize {
    let Ok(tasks) = d.store.pending_tasks() else {
        return 0;
    };
    let mut resumed = 0;
    for task in tasks {
        let Ok(session) = d.sessions.get(&task.session) else {
            // The session is gone; there is nothing to resume into.
            let _ = d.store.delete_pending_task(task.id);
            continue;
        };
        // Kept for the log only: the turn is replayed from the transcript, not
        // from this payload, so the message must not be appended twice.
        let message = task
            .payload
            .get("message")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string();
        // An "inject" row was never delivered: it was written while a turn was
        // running, and that turn (or the process) ended before the step
        // boundary that would have folded it in. Write it now -- the resume
        // below replays a turn that reads the transcript, and a message that is
        // not in the transcript would be replayed into nothing.
        if task.kind == "inject" {
            if !message.is_empty() {
                if let Err(e) = d.store.append_message(
                    session.id(),
                    "text",
                    &json!({"role": "user", "text": message}),
                ) {
                    tracing::warn!(session = %task.session, error = %e, "could not deliver a pending message");
                }
            }
        } else if task.kind != "turn" {
            // Not a kind this loop knows how to replay.
            let _ = d.store.delete_pending_task(task.id);
            continue;
        }
        tracing::info!(session = %task.session, message = %message, "replaying a turn left pending");
        let agent = d.agent.clone();
        let store = d.store.clone();
        let active = d.active.clone();
        // Only this task is finished. Clearing the whole session would drop
        // tasks a second, still-pending turn of the same session recorded.
        let task_id = task.id;
        active.fetch_add(1, Ordering::SeqCst);
        tokio::spawn(async move {
            agent.resume_turn(&session, Some(task_id)).await;
            let _ = store.delete_pending_task(task_id);
            active.fetch_sub(1, Ordering::SeqCst);
        });
        resumed += 1;
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
    fn catalog_is_exactly_the_known_commands() {
        let catalog = catalog();
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
    }

    #[test]
    fn session_args_are_completable() {
        let spec = catalog()
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
        let spec = catalog().into_iter().find(|c| c.name == "cancel").unwrap();
        assert!(spec.args.is_empty(), "cancel is a whole-server stop");
    }

    /// Interrupting is per session, and the session list is what completes it.
    /// The name is required: guessing which session to stop is exactly the
    /// hidden state the command surface exists to avoid.
    #[test]
    fn interrupt_session_names_one_session() {
        let spec = catalog()
            .into_iter()
            .find(|c| c.name == "interrupt_session")
            .unwrap();
        let name = spec.args.iter().find(|a| a.name == "session").unwrap();
        assert!(name.required, "--session is required");
        assert!(matches!(name.completions, CompletionSource::Sessions));
    }

    /// Every command names its session the same way. Four spellings of the
    /// same argument is how a sticky `session` ends up filling only three of
    /// them.
    #[test]
    fn every_command_takes_a_session_or_a_message() {
        for spec in catalog() {
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

    /// The catalog is a wire object read by the client's completer, so every
    /// source it declares has to survive the trip. A value list that lost its
    /// variant on the way out would make Tab silently offer nothing.
    #[test]
    fn completion_sources_survive_the_wire() {
        let json = serde_json::to_string(&catalog()).unwrap();
        let back: Vec<CommandSpec> = serde_json::from_str(&json).unwrap();
        let source = |command: &str, arg: &str| {
            back.iter()
                .find(|c| c.name == command)
                .and_then(|c| c.args.iter().find(|a| a.name == arg))
                .map(|a| a.completions.clone())
                .unwrap_or(CompletionSource::None)
        };
        assert!(matches!(source("send_message", "session"), CompletionSource::Sessions));
        assert!(matches!(source("set_session", "cwd"), CompletionSource::Paths));
        assert!(matches!(
            source("set_session", "compact_threshold"),
            CompletionSource::Static(_)
        ));
        // Free-form text stays free-form: the catalogue says nothing, and the
        // client must not invent a list for it.
        assert!(matches!(source("send_message", "message"), CompletionSource::None));
    }

    /// `set_session` is where a session's model and compaction point are
    /// chosen, so both flags have to be described well enough to complete.
    #[test]
    fn set_session_offers_the_roster_and_a_percentage() {
        let spec = catalog()
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

    /// Reading is scoped to a session and paged by sequence number: an IM
    /// bridge asks "what appeared after the seq I left off at", so a wrong
    /// default here would make it resend old answers.
    #[test]
    fn get_messages_pages_by_seq() {
        let spec = catalog()
            .into_iter()
            .find(|c| c.name == "get_messages")
            .unwrap();
        let session = spec.args.iter().find(|a| a.name == "session").unwrap();
        assert!(session.required);
        assert!(matches!(session.completions, CompletionSource::Sessions));
        let after = spec.args.iter().find(|a| a.name == "after_seq").unwrap();
        assert!(!after.required, "0 is a valid starting point");
        assert!(!spec.args.iter().any(|a| a.name == "limit" && a.required));
    }
}
