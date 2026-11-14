//! SPEC's monitor: the sessions the harness hands a report to.
//!
//! See `docs/notes/server-monitor.md`.
//!
//! # Invariant
//!
//! A monitor is an ordinary session. No name is reserved, no model is picked for
//! it, and no failure switches it: what it runs on is what its own row says. A
//! monitor that fails is a session in `error`, drawn like any other.

use std::path::Path;
use std::sync::Arc;

use protocol::LogLevel;

use crate::commands::Dispatch;
use crate::store::Failure;

/// One kind of monitor: the session a report of it is given, and the skill that
/// session's standing knowledge comes from.
///
/// # Invariant
///
/// One shape for every kind, so how a report reaches a session is one
/// implementation and not one per kind. Kinds differ only in what they watch and
/// what a report says.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Kind {
    /// The session a report of this kind is given.
    pub session: &'static str,
    /// The skill the report references: its words ride the prompt's prefix.
    pub skill: &'static str,
}

/// A session stopped because a provider refused, or stopped answering, its request.
///
/// # Invariant
///
/// This is the only error kind: SPEC's monitor watches the machine's providers, so
/// a session that broke for any other reason is not handed to it. What it is told
/// to reach is the `providers` skill -- the roster, the keys, the quota scripts and
/// how to prove which of them is wrong.
pub const PROVIDERS: Kind = Kind {
    session: "@monitor/providers",
    skill: "providers",
};

/// This device ran out of room.
pub const DISK: Kind = Kind {
    session: "@monitor/disk",
    skill: "clean-disk",
};

/// How many rows of the failing transcript a report opens with.
const CONTEXT_ROWS: i64 = 20;

/// The event the disk kind has already handed over.
const DISK_EVENT_CURSOR: &str = "disk.event";

/// The quota event already handed over.
const QUOTA_EVENT_CURSOR: &str = "quota.event";

/// What a report is about, in one line: the monitor reads the same skill either way.
const SESSION_WHAT: &str = "A gongga session stopped because a provider would not serve it";
const QUOTA_WHAT: &str = "This machine could not read a provider's allowance";

/// Hands every unwatched trigger to the monitor of its kind.
///
/// # Invariant
///
/// The store is the truth, not the bus: a turn that ended while this process was not
/// running is in `sessions.status`, and its event in the log. Called at start-up and
/// on the tick that wakes inboxes, so neither order loses a report.
pub async fn sweep(d: &Arc<Dispatch>, home: &Path) {
    if d.shutting_down.load(std::sync::atomic::Ordering::SeqCst) || d.shutdown.is_cancelled() {
        return;
    }
    sweep_errors(d, home).await;
    sweep_quota(d, home).await;
    sweep_disk(d, home).await;
}

/// Every session in `error` that a report is still owed to.
async fn sweep_errors(d: &Arc<Dispatch>, home: &Path) {
    let Ok(failures) = d.store.failures() else {
        return;
    };
    for failure in failures {
        // Reported already: the handed-over row is the whole record of that.
        if failure.last_seq <= failure.monitor_seq {
            continue;
        }
        // Never told about its own failure: the turn it just failed *was* a
        // report, so another would be the same one forever.
        if failure.name == PROVIDERS.session {
            continue;
        }
        report_failure(d, home, &failure).await;
    }
}

/// One failure: give it to `PROVIDERS`'s session.
async fn report_failure(d: &Arc<Dispatch>, home: &Path, failure: &Failure) {
    let handed = hand_over(d, PROVIDERS, |mine| report(d, home, failure, mine)).await;
    match handed {
        Handed::Accepted => {
            tracing::info!(session = %failure.name, "handed a failure to the monitor");
            // After, not before: a crash between the two repeats a report, which is
            // recoverable, where the other order loses one.
            let _ = d.store.mark_monitored(failure.id, failure.last_seq);
        }
        Handed::Failed(why) => {
            // Waiting changes nothing here, so it is said once and recorded as
            // delivered rather than again on every tick.
            d.bus.publish(protocol::Event::Log {
                level: LogLevel::Error,
                message: why,
                session: None,
            });
            let _ = d.store.mark_monitored(failure.id, failure.last_seq);
        }
        Handed::Refused(why) => {
            tracing::warn!(session = %failure.name, why, "the monitor did not take the report");
        }
    }
}

/// Every allowance this machine could not read and no monitor has been told about.
///
/// # Invariant
///
/// One wake per event row, and the row is the whole memory: the cursor is what this
/// server has handed over, so a restart reads the event log rather than asking the
/// provider again. The event is written once per distinct reason (`quota::publish`),
/// so a provider that keeps failing is one wake and not one per step.
async fn sweep_quota(d: &Arc<Dispatch>, home: &Path) {
    let cursor = d.store.monitor_value(QUOTA_EVENT_CURSOR).unwrap_or(0);
    let Ok(rows) = d
        .store
        .events_after(cursor, Some(crate::quota::EVENT), None, 8)
    else {
        return;
    };
    for row in rows {
        give_quota(d, home, &row).await;
    }
}

/// Hands one unreadable allowance to the monitor, and records what came of it.
async fn give_quota(d: &Arc<Dispatch>, home: &Path, row: &crate::store::EventRow) {
    let payload = row.payload.clone();
    match hand_over(d, PROVIDERS, move |mine| {
        Ok(quota_report(home, &payload, mine))
    })
    .await
    {
        Handed::Accepted => {
            tracing::info!(
                event = row.id,
                "handed an unreadable allowance to the monitor"
            );
            let _ = d.store.set_monitor_value(QUOTA_EVENT_CURSOR, row.id);
        }
        Handed::Failed(why) => {
            d.bus.publish(protocol::Event::Log {
                level: LogLevel::Error,
                message: why,
                session: None,
            });
            let _ = d.store.set_monitor_value(QUOTA_EVENT_CURSOR, row.id);
        }
        Handed::Refused(why) => {
            tracing::warn!(
                event = row.id,
                why,
                "the monitor did not take the quota report"
            );
        }
    }
}

/// The report for one `crate::quota::EVENT`; the prompt carries the rules.
fn quota_report(home: &Path, payload: &str, mine: &str) -> String {
    let facts: serde_json::Value = serde_json::from_str(payload).unwrap_or(serde_json::Value::Null);
    let text = |key: &str| {
        facts
            .get(key)
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string()
    };
    let provider = text("provider");
    crate::prompts::shared().render(
        crate::prompts::MONITOR_PROVIDERS,
        &[
            ("what", QUOTA_WHAT),
            ("session", &format!("quota:{provider}")),
            ("model", &provider),
            (
                "rows",
                "(no transcript: nothing ran, the allowance could not be read)",
            ),
            ("build", &protocol::version_line()),
            ("home", &home.display().to_string()),
            ("pid", &std::process::id().to_string()),
            (
                "cwd",
                &std::env::current_dir()
                    .map(|p| p.to_string_lossy().into_owned())
                    .unwrap_or_default(),
            ),
            ("mine", mine),
            (
                "noted",
                &format!("\nWhat the reader said:\n\n{}\n", text("note")),
            ),
        ],
    )
}

/// Every disk event no monitor has been told about yet.
///
/// # Invariant
///
/// One wake per event row, and the row is the whole memory: the room was read once
/// for it, and a restart reads the event log rather than the disk.
async fn sweep_disk(d: &Arc<Dispatch>, home: &Path) {
    let cursor = d.store.monitor_value(DISK_EVENT_CURSOR).unwrap_or(0);
    let Ok(rows) = d
        .store
        .events_after(cursor, Some(crate::disk::EVENT), None, 8)
    else {
        return;
    };
    for row in rows {
        give_disk(d, home, &row).await;
    }
}

/// Hands one disk event to the monitor, and records what came of it.
async fn give_disk(d: &Arc<Dispatch>, home: &Path, row: &crate::store::EventRow) {
    let payload = row.payload.clone();
    match hand_over(d, DISK, move |_| Ok(disk_report(home, &payload))).await {
        Handed::Accepted => {
            tracing::info!(event = row.id, "handed the disk's last room to the monitor");
            let _ = d.store.set_monitor_value(DISK_EVENT_CURSOR, row.id);
        }
        Handed::Failed(why) => {
            d.bus.publish(protocol::Event::Log {
                level: LogLevel::Error,
                message: why,
                session: None,
            });
            let _ = d.store.set_monitor_value(DISK_EVENT_CURSOR, row.id);
        }
        Handed::Refused(why) => {
            tracing::warn!(
                event = row.id,
                why,
                "the monitor did not take the disk report"
            );
        }
    }
}

/// What one handover did.
enum Handed {
    /// The session took it; the caller records that.
    Accepted,
    /// It never will: no session could be made, or no report written.
    Failed(String),
    /// Not now: the session would not take it. The next sweep asks again.
    Refused(String),
}

/// Gives `kind`'s session the report `write` produces, making the session first
/// when it has none.
///
/// # Invariant
///
/// One path for every kind, and it is `commands::deliver`: the queue gate,
/// `@skill(...)` expansion, the tools and the window's rendering are reused rather
/// than re-implemented. `write` is handed the model the session runs on, so a
/// report can say what its own turn runs on after a person configured it.
async fn hand_over<F>(d: &Dispatch, kind: Kind, write: F) -> Handed
where
    F: FnOnce(&str) -> anyhow::Result<String>,
{
    let mine = match ensure(d, kind).await {
        Ok(model) => model,
        Err(why) => return Handed::Failed(why),
    };
    let report = match write(&mine) {
        Ok(text) => crate::prompts::harness_wrap(text),
        Err(e) => return Handed::Failed(format!("cannot write the report: {e}")),
    };
    match crate::commands::deliver(d, kind.session, report).await {
        outcome if outcome.ok => Handed::Accepted,
        outcome => Handed::Refused(outcome.message),
    }
}

/// The model `kind`'s session runs on, creating the session when it has none.
///
/// # Invariant
///
/// No creation path of its own: `commands::create_session` is `set_session` with a
/// name and nothing else, so a monitor starts on what a session a person named
/// starts on. An existing one is never written to here.
async fn ensure(d: &Dispatch, kind: Kind) -> Result<String, String> {
    let found = d
        .store
        .get_session(kind.session)
        .map_err(|e| e.to_string())?;
    if let Some(row) = found {
        return Ok(row.model);
    }
    crate::commands::create_session(d, kind.session).await?;
    d.store
        .get_session(kind.session)
        .map_err(|e| e.to_string())?
        .map(|row| row.model)
        .ok_or_else(|| format!("{} was created and cannot be read back", kind.session))
}

/// What the error report says: enough to start work without reading anything first.
fn report(d: &Dispatch, home: &Path, failure: &Failure, mine: &str) -> anyhow::Result<String> {
    let written = last_notice(d, failure.id)?;
    // Only this failure varies; the map and the rules are the template's.
    let noted = if written.is_empty() {
        String::new()
    } else {
        format!("\nThe last thing the server recorded for it:\n\n{written}\n")
    };
    Ok(crate::prompts::shared().render(
        crate::prompts::MONITOR_PROVIDERS,
        &[
            ("what", SESSION_WHAT),
            ("session", &failure.name),
            ("model", &failure.model),
            (
                "rows",
                &format!(
                    "{}..{} ({} rows in the transcript)",
                    (failure.last_seq - CONTEXT_ROWS).max(0),
                    failure.last_seq,
                    failure.last_seq
                ),
            ),
            ("build", &protocol::version_line()),
            ("home", &home.display().to_string()),
            ("pid", &std::process::id().to_string()),
            (
                "cwd",
                &std::env::current_dir()
                    .map(|p| p.to_string_lossy().into_owned())
                    .unwrap_or_default(),
            ),
            ("mine", mine),
            ("noted", &noted),
        ],
    ))
}

/// What the disk report says: the room left, and the volumes as they stood.
fn disk_report(home: &Path, payload: &str) -> String {
    let facts: serde_json::Value = serde_json::from_str(payload).unwrap_or(serde_json::Value::Null);
    let number = |key: &str| -> Option<u64> { facts.get(key).and_then(|v| v.as_u64()) };
    let text = |key: &str| -> String {
        facts
            .get(key)
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string()
    };
    let volumes = facts
        .get("volumes")
        .and_then(|v| v.as_array())
        .map(|list| {
            list.iter()
                .map(|volume| {
                    format!(
                        "  - {}: {}",
                        volume.get("label").and_then(|v| v.as_str()).unwrap_or("?"),
                        volume
                            .get("free")
                            .and_then(|v| v.as_u64())
                            .map(crate::disk::human)
                            .unwrap_or_else(|| "not reported".to_string()),
                    )
                })
                .collect::<Vec<_>>()
                .join("\n")
        })
        .unwrap_or_else(|| "  (nothing was measured)".to_string());
    let machine = text("machine");
    let elsewhere = machine != crate::disk::LOCAL;
    // A peer's disk is cleaned through `remote`: the cleaner is a file here.
    let prefix = protocol::Reference::Skill.prefix();
    let skills = if elsewhere {
        format!("{prefix}{})\n{prefix}remote)", DISK.skill)
    } else {
        format!("{prefix}{})", DISK.skill)
    };
    let session = text("session");
    let read_back = if session.is_empty() {
        String::new()
    } else {
        format!(
            "\nRead it back with:\n\n    gongga-cli '{{\"name\":\"get_messages\",\"args\":{{\"session\":\"{session}\",\"range\":\"-40:-1\"}}}}'\n"
        )
    };
    let reach = if elsewhere {
        format!(
            "\nThe disk that is low is {machine}'s, and this turn's shells are this \
             machine's: reach it with the `remote` skill (`remote run {machine} <command>`, \
             `remote put`/`remote get` for files) before cleaning anything there.\n"
        )
    } else {
        String::new()
    };
    crate::prompts::shared().render(
        crate::prompts::MONITOR_DISK,
        &[
            ("skills", &skills),
            ("machine", &machine),
            ("home", &home.display().to_string()),
            ("volumes", &volumes),
            (
                "threshold",
                &number("threshold")
                    .map(crate::disk::human)
                    .unwrap_or_else(|| "unknown".to_string()),
            ),
            ("tool", &text("tool")),
            ("session", &session),
            (
                "ok",
                &if facts
                    .get("tool_ok")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(true)
                {
                    "which succeeded".to_string()
                } else {
                    "which failed".to_string()
                },
            ),
            ("cwd", &text("cwd")),
            ("build", &protocol::version_line()),
            ("pid", &std::process::id().to_string()),
            (
                "server_cwd",
                &std::env::current_dir()
                    .map(|p| p.to_string_lossy().into_owned())
                    .unwrap_or_default(),
            ),
            ("read_back", &read_back),
            ("reach", &reach),
        ],
    )
}

/// The last thing the server recorded for a session, which is the reason.
fn last_notice(d: &Dispatch, id: i64) -> anyhow::Result<String> {
    let last = d.store.last_message_seq(id)?;
    let tail = d
        .store
        .messages_between(id, (last - CONTEXT_ROWS).max(0), last)?;
    Ok(tail
        .messages
        .iter()
        .rev()
        .find(|row| row.kind == "notice" && !row.text.trim().is_empty())
        .map(|row| row.text.trim().to_string())
        .unwrap_or_default())
}
