//! The command surface: catalog and dispatch.
//!
//! The server answers four commands, which is all the SPEC asks of it:
//! `send_message`, `set_session`, `del_session` and `cancel`. Sticky arguments
//! are a client-side concern and never appear here.
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

pub struct Dispatch {
    pub agent: Arc<Agent>,
    pub sessions: SessionManager,
    pub store: Store,
    pub bus: EventBus,
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
            summary: "Create the session if it does not exist.".into(),
            args: vec![
                arg("name", "Session name.", true, Some("name"), CompletionSource::None),
                arg(
                    "cwd",
                    "Working directory for a newly created session. Defaults to the server's.",
                    false,
                    Some("path"),
                    CompletionSource::None,
                ),
            ],
        },
        CommandSpec {
            name: "del_session".into(),
            summary: "Delete a session, its shells and its transcript.".into(),
            args: vec![arg(
                "name",
                "Session to delete.",
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
        "cancel" => cancel(d).await,
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
    // Remember which row is this message's. Clearing by session used to delete
    // the rows of messages still waiting their turn: the first turn finished
    // and took the second one's pending row with it, so a crash before the
    // second turn ran lost the message entirely -- it was neither in the
    // transcript nor in the pending list.
    let task_id = match d
        .store
        .add_pending_task(&label, "turn", &json!({"session": label, "message": message}))
    {
        Ok(id) => id,
        Err(e) => return Outcome::err(format!("could not record the pending turn: {e}")),
    };

    let agent = d.agent.clone();
    let store = d.store.clone();
    let active = d.active.clone();
    let session_resumable = session.clone();
    active.fetch_add(1, Ordering::SeqCst);
    tokio::spawn(async move {
        agent.run_turn(&session, &message).await;
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
    let name = args["name"].clone();
    // `set_session` doubles as creation: naming a session is the only way to
    // get one, and refusing to create it would make the command useless.
    match d.sessions.ensure(&name, args.get("cwd").map(String::as_str)) {
        Ok(session) => {
            publish_sessions(d);
            let cwd = session.cwd().await;
            Outcome::ok_with(
                format!("session is now {name}"),
                json!({"name": name, "cwd": cwd}),
            )
        }
        Err(e) => Outcome::err(e.to_string()),
    }
}

async fn del_session(d: &Dispatch, args: &Args) -> Outcome {
    let name = args["name"].clone();
    // Deleting a session whose turn is mid-flight would take the transcript
    // out from under it -- and the turn would keep writing to a session that
    // no longer exists. So wait for the session to be idle first. A session
    // with no handle in memory has nothing in flight, so there is nothing to
    // wait for and no reason to materialise it just to delete it.
    let _gate = match d.sessions.peek(&name) {
        Some(session) => {
            if session.busy() {
                // Without this the command looks hung: it blocks until the
                // turn in front of it finishes, which can be minutes.
                tracing::info!(session = %name, "del_session waiting for the session to be free");
            }
            Some(session.claim().await)
        }
        None => None,
    };
    match d.sessions.delete(&name) {
        Ok(true) => {
            // Deleting a session is the one thing that means "the work it
            // started is not wanted either". `cancel` disagrees on purpose:
            // detached jobs exist to outlive the server's own shutdown.
            let stopped = d.sessions.detached().stop_session(&name).await;
            publish_sessions(d);
            Outcome::ok(if stopped.total() == 0 {
                format!("deleted session {name}")
            } else {
                format!("deleted session {name} and stopped {} detached job(s)", stopped.stopped)
            })
        }
        Ok(false) => Outcome::err(format!("no session named {name:?}")),
        Err(e) => Outcome::err(e.to_string()),
    }
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
        if task.kind != "turn" {
            let _ = d.store.delete_pending_task(task.id);
            continue;
        }
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
        tracing::info!(session = %task.session, message = %message, "replaying a turn left pending");
        let agent = d.agent.clone();
        let store = d.store.clone();
        let active = d.active.clone();
        // Only this task is finished. Clearing the whole session would drop
        // tasks a second, still-pending turn of the same session recorded.
        let task_id = task.id;
        active.fetch_add(1, Ordering::SeqCst);
        tokio::spawn(async move {
            agent.resume_turn(&session).await;
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
    fn catalog_is_exactly_the_spec_commands() {
        let catalog = catalog();
        let mut names: Vec<&str> = catalog.iter().map(|c| c.name.as_str()).collect();
        names.sort();
        assert_eq!(names, vec!["cancel", "del_session", "send_message", "set_session"]);
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

    #[test]
    fn every_command_takes_a_name_or_message() {
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
                required.contains(&"name") || required.contains(&"message"),
                "{} requires one of name/message",
                spec.name
            );
        }
    }
}
