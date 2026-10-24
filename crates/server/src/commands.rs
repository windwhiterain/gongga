//! The command surface: catalog and dispatch.
//! See `docs/notes/server-commands.md`.

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

/// Default `get_messages` window: one whole turn, not the entire history.
const DEFAULT_MESSAGE_LIMIT: usize = 50;
/// Cap, so a typo cannot pull megabytes of transcript through a UDP channel
/// that fragments at 1174 bytes a packet.
const MAX_MESSAGE_LIMIT: usize = 500;

pub struct Dispatch {
    pub agent: Arc<Agent>,
    pub sessions: SessionManager,
    pub store: Store,
    pub bus: EventBus,
    /// The machine's LLM roster: what `set_session --llm` is checked against.
    pub providers: &'static crate::providers::Catalog,
    /// Turns currently running. `cancel` waits for this to drain before the
    /// process is allowed to exit.
    pub active: Arc<AtomicUsize>,
    /// Pulled by `cancel`; the listen loop stops accepting work when it fires.
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

/// The result of one command, sent back as a `cmd_result` event.
pub struct Outcome {
    pub ok: bool,
    pub message: String,
    pub data: Option<serde_json::Value>,
}

impl Outcome {
    fn ok(message: impl Into<String>) -> Self {
        Self {
            ok: true,
            message: message.into(),
            data: None,
        }
    }

    fn ok_with(message: impl Into<String>, data: serde_json::Value) -> Self {
        Self {
            ok: true,
            message: message.into(),
            data: Some(data),
        }
    }

    fn err(message: impl Into<String>) -> Self {
        Self {
            ok: false,
            message: message.into(),
            data: None,
        }
    }
}

pub fn catalog() -> Vec<CommandSpec> {
    vec![
        CommandSpec {
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
    // No implicit session: a default is the client's business, not hidden state.
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
    // One read of `busy` picks the kind and whether a turn starts; see `docs/notes/server-commands.md`.
    let busy = session.busy();
    let kind = if busy { "inject" } else { "turn" };
    // Keep this message's own row id; clearing by session would take others with it.
    let task_id =
        match d
            .store
            .add_pending_task(&label, kind, &json!({"session": label, "message": message}))
        {
            Ok(id) => id,
            Err(e) => return Outcome::err(format!("could not record the pending turn: {e}")),
        };

    if busy {
        // Whoever is waiting inside this turn hears about it; see the notes.
        session.note_message();
        // The running turn folds this in; `nudge_inboxes` covers it ending first.
        return Outcome::ok(format!(
            "message queued for {label}; the running command has been told to stop waiting"
        ));
    }

    let agent = d.agent.clone();
    let store = d.store.clone();
    let active = d.active.clone();
    let session_resumable = session.clone();
    tokio::spawn(async move {
        let _active = ActiveGuard::new(active);
        agent.run_turn(&session, &message, Some(task_id)).await;
        // Done means the next start owes nothing, unless a `cancel` kept it pending.
        if !session_resumable.take_resumable() {
            let _ = store.delete_pending_task(task_id);
        }
    });
    Outcome::ok(format!("message accepted by {label}"))
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
    // Delete means "not this work either", so interrupt before waiting for the gate.
    let (was_busy, dropped) = interrupt(d, &name);
    // The rows cannot go until the turn lets go, so take the gate after interrupting.
    let _gate = match d.sessions.peek(&name) {
        Some(session) => Some(session.claim().await),
        None => None,
    };
    match d.sessions.delete(&name) {
        Ok(true) => {
            // Delete stops detached jobs too; `cancel` keeps them, by design.
            let stopped = d.sessions.detached().stop_session(&name).await;
            publish_sessions(d);
            let mut message = if stopped.total() == 0 {
                format!("deleted session {name}")
            } else {
                format!(
                    "deleted session {name} and stopped {} detached job(s)",
                    stopped.stopped
                )
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
         detached job(s) keep running -- they outlive the server, stop them with `kill_bash`",
        running.len(),
        pending,
        detached
    ))
}

/// Wakes sessions holding a message nobody is left to read.
///
/// # Invariant
///
/// Only "inject" rows are woken: a "turn" row already belongs to a turn that is
/// starting or queued.
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
        // `None`: this turn owns no row, so the drain may take everything queued.
        tokio::spawn(async move {
            let _active = ActiveGuard::new(active);
            agent.resume_turn(&session, None).await;
        });
        started += 1;
    }
    started
}

/// Replays interrupted turns once the server is back, from their transcript.
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
        // Log only: the turn replays from the transcript, so do not append it twice.
        let message = task
            .payload
            .get("message")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string();
        // An "inject" row was never delivered; write it, or the resume replays nothing.
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
        // Clear only this task; the session may hold a second pending turn's row.
        let task_id = task.id;
        tokio::spawn(async move {
            let _active = ActiveGuard::new(active);
            agent.resume_turn(&session, Some(task_id)).await;
            let _ = store.delete_pending_task(task_id);
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

    /// Interrupting names one session; guessing one is the hidden state we avoid.
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

    /// Every command spells its session the same way, so sticky `session` works.
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

    /// Every completion source must survive the wire, or Tab silently offers nothing.
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

    /// Reading is paged by seq, so a wrong default would make a bridge resend.
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
