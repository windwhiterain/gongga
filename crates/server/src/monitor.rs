//! SPEC's monitor: the sessions the harness spawns and hands a report to.
//!
//! See `docs/notes/server-monitor.md`.

use std::path::Path;
use std::sync::Arc;

use protocol::{LogLevel, SessionStatus};

use crate::commands::Dispatch;
use crate::providers::Entry;
use crate::store::Failure;

/// One kind of monitor: the session it wakes, and the skill that session's
/// standing knowledge comes from.
///
/// # Invariant
///
/// One shape for every kind, so which model a monitor runs on, how it is
/// re-chosen when it fails, and how a report reaches it are one implementation and
/// not one per kind. Kinds differ only in what they watch and what a report says.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Kind {
    /// The session, inside the harness's reserved `@` namespace.
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

/// Why `name` may not be a person's session, or `None` when it may.
///
/// # Invariant
///
/// Refused, not renamed: a session under a name the caller did not choose is a
/// session they will not find again. Deleting one is allowed -- a monitor that
/// cannot work has to be removable. Every segment is checked, not just the whole
/// name: `work/@monitor/error` draws a row the harness cannot tell from its own.
pub fn reserved(name: &str) -> Option<String> {
    let taken = name.split('/').any(|segment| segment.starts_with('@'));
    taken.then(|| {
        format!(
            "names beginning with `@` belong to the harness ({PROVIDERS} and {DISK} are two), \
             whole or by segment; pick a name without it",
            PROVIDERS = PROVIDERS.session,
            DISK = DISK.session,
        )
    })
}

/// Whether `name` is one of the harness's own, which a failure of is not a
/// failure of somebody's work.
fn is_monitor(name: &str) -> bool {
    name.starts_with("@monitor/")
}

/// How many rows of the failing transcript a report opens with.
const CONTEXT_ROWS: i64 = 20;

/// The event the disk kind has already handed over.
const DISK_EVENT_CURSOR: &str = "disk.event";
/// How many models that handover has been through.
const DISK_EVENT_ATTEMPTS: &str = "disk.attempts";
/// The row the last disk report became: a monitor in `error` has written past it.
const DISK_WAKE_ROW: &str = "disk.wake";

/// The quota event already handed over, how many models that took, and the row the
/// last such report became.
const QUOTA_EVENT_CURSOR: &str = "quota.event";
const QUOTA_EVENT_ATTEMPTS: &str = "quota.attempts";
const QUOTA_WAKE_ROW: &str = "quota.wake";

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

/// Every session in `error` that a monitor turn still owes a report to.
async fn sweep_errors(d: &Arc<Dispatch>, home: &Path) {
    let Ok(failures) = d.store.failures() else {
        return;
    };
    for failure in failures {
        // Reported already: the handed-over row is the whole record of that.
        if failure.last_seq <= failure.monitor_seq {
            continue;
        }
        report_failure(d, home, &failure, is_monitor(&failure.name)).await;
    }
}

/// One failure: choose a model for the monitor, and give it the report.
async fn report_failure(d: &Arc<Dispatch>, home: &Path, failure: &Failure, self_failed: bool) {
    let attempts = failure.monitor_attempts;
    // The attempts made so far are also the index of the next candidate.
    match hand_over(d, PROVIDERS, attempts, |entry| {
        report(d, home, failure, self_failed, entry)
    })
    .await
    {
        Handed::Accepted(model) => {
            tracing::info!(
                session = %failure.name,
                model = %model,
                attempt = attempts.saturating_add(1),
                "handed a failure to the monitor"
            );
            // After, not before: a crash between the two repeats a report, which is
            // recoverable, where the other order loses one.
            let _ =
                d.store
                    .mark_monitored(failure.id, failure.last_seq, attempts.saturating_add(1));
        }
        Handed::GivenUp(why) => {
            // Recorded as delivered so the search does not run again every tick.
            d.bus.publish(protocol::Event::Log {
                level: LogLevel::Error,
                message: why,
            });
            let _ = d
                .store
                .mark_monitored(failure.id, failure.last_seq, attempts);
        }
        Handed::Refused(why) => {
            tracing::warn!(session = %failure.name, why, "the monitor did not take the report");
        }
    }
}

/// Every allowance this machine could not read, and the report a monitor that failed
/// on its model is owed.
///
/// # Invariant
///
/// Same shape as the disk sweep: one wake per event row, the row is the whole
/// memory, and a monitor that died on its model is given the same report again on
/// the next one. The event is written once per distinct reason (`quota::publish`),
/// so a provider that keeps failing is one wake and not one per step.
async fn sweep_quota(d: &Arc<Dispatch>, home: &Path) {
    let cursor = d.store.monitor_value(QUOTA_EVENT_CURSOR).unwrap_or(0);
    let attempts = d
        .store
        .monitor_value(QUOTA_EVENT_ATTEMPTS)
        .unwrap_or(0)
        .max(0) as u32;
    let wake = d.store.monitor_value(QUOTA_WAKE_ROW).unwrap_or(0);
    if wake > 0
        && cursor > 0
        && let Ok(Some(row)) = d.store.event_row(cursor)
        && let Ok(Some(session)) = d.store.get_session(PROVIDERS.session)
        && session.status == SessionStatus::Error
    {
        give_quota(d, home, &row, attempts.saturating_add(1)).await;
        return;
    }
    let Ok(rows) = d
        .store
        .events_after(cursor, Some(crate::quota::EVENT), None, 8)
    else {
        return;
    };
    for row in rows {
        give_quota(d, home, &row, attempts).await;
    }
}

/// Hands one unreadable allowance to the monitor, and records what came of it.
async fn give_quota(d: &Arc<Dispatch>, home: &Path, row: &crate::store::EventRow, attempts: u32) {
    let payload = row.payload.clone();
    match hand_over(d, PROVIDERS, attempts, move |_| {
        Ok(quota_report(home, &payload))
    })
    .await
    {
        Handed::Accepted(model) => {
            tracing::info!(
                event = row.id,
                model = %model,
                attempt = attempts.saturating_add(1),
                "handed an unreadable allowance to the monitor"
            );
            let _ = d.store.set_monitor_value(QUOTA_EVENT_CURSOR, row.id);
            let _ = d.store.set_monitor_value(QUOTA_EVENT_ATTEMPTS, 0);
            if let Ok(Some(session)) = d.store.get_session(PROVIDERS.session)
                && let Ok(seq) = d.store.last_message_seq(session.id)
            {
                let _ = d.store.set_monitor_value(QUOTA_WAKE_ROW, seq);
            }
        }
        Handed::GivenUp(why) => {
            d.bus.publish(protocol::Event::Log {
                level: LogLevel::Error,
                message: why,
            });
            let _ = d.store.set_monitor_value(QUOTA_EVENT_CURSOR, row.id);
            let _ = d.store.set_monitor_value(QUOTA_EVENT_ATTEMPTS, 0);
            let _ = d.store.set_monitor_value(QUOTA_WAKE_ROW, 0);
        }
        Handed::Refused(why) => {
            tracing::warn!(
                event = row.id,
                why,
                "the monitor did not take the quota report"
            );
            let _ = d
                .store
                .set_monitor_value(QUOTA_EVENT_ATTEMPTS, i64::from(attempts.saturating_add(1)));
        }
    }
}

/// The report for one `crate::quota::EVENT`; the prompt carries the rules.
fn quota_report(home: &Path, payload: &str) -> String {
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
            ("mine", ""),
            (
                "noted",
                &format!("\nWhat the reader said:\n\n{}\n", text("note")),
            ),
        ],
    )
}

/// Every disk event no monitor has been told about yet, and the report a monitor
/// that failed on its model is owed.
///
/// # Invariant
///
/// One wake per event row, and the row is the whole memory: the room was read once
/// for it, and a restart reads the event log rather than the disk. A monitor that
/// failed gets the same report on the next model, so a refused first choice is still
/// cleaned.
async fn sweep_disk(d: &Arc<Dispatch>, home: &Path) {
    let cursor = d.store.monitor_value(DISK_EVENT_CURSOR).unwrap_or(0);
    let attempts = d
        .store
        .monitor_value(DISK_EVENT_ATTEMPTS)
        .unwrap_or(0)
        .max(0) as u32;
    let wake = d.store.monitor_value(DISK_WAKE_ROW).unwrap_or(0);
    // A report already handed over, on a session that ended in error: the same
    // report again, on the next model.
    if wake > 0
        && cursor > 0
        && let Ok(Some(row)) = d.store.event_row(cursor)
        && let Ok(Some(session)) = d.store.get_session(DISK.session)
        && session.status == SessionStatus::Error
    {
        give_disk(d, home, &row, attempts.saturating_add(1)).await;
        return;
    }
    let Ok(rows) = d
        .store
        .events_after(cursor, Some(crate::disk::EVENT), None, 8)
    else {
        return;
    };
    for row in rows {
        give_disk(d, home, &row, attempts).await;
    }
}

/// Hands one disk event to the monitor, and records what came of it.
async fn give_disk(d: &Arc<Dispatch>, home: &Path, row: &crate::store::EventRow, attempts: u32) {
    let payload = row.payload.clone();
    match hand_over(d, DISK, attempts, move |_| Ok(disk_report(home, &payload))).await {
        Handed::Accepted(model) => {
            tracing::info!(
                event = row.id,
                model = %model,
                attempt = attempts.saturating_add(1),
                "handed the disk's last room to the monitor"
            );
            let _ = d.store.set_monitor_value(DISK_EVENT_CURSOR, row.id);
            let _ = d.store.set_monitor_value(DISK_EVENT_ATTEMPTS, 0);
            if let Ok(Some(session)) = d.store.get_session(DISK.session)
                && let Ok(seq) = d.store.last_message_seq(session.id)
            {
                let _ = d.store.set_monitor_value(DISK_WAKE_ROW, seq);
            }
        }
        Handed::GivenUp(why) => {
            d.bus.publish(protocol::Event::Log {
                level: LogLevel::Error,
                message: why,
            });
            let _ = d.store.set_monitor_value(DISK_EVENT_CURSOR, row.id);
            let _ = d.store.set_monitor_value(DISK_EVENT_ATTEMPTS, 0);
            let _ = d.store.set_monitor_value(DISK_WAKE_ROW, 0);
        }
        Handed::Refused(why) => {
            tracing::warn!(
                event = row.id,
                why,
                "the monitor did not take the disk report"
            );
            let _ = d
                .store
                .set_monitor_value(DISK_EVENT_ATTEMPTS, i64::from(attempts.saturating_add(1)));
        }
    }
}

/// What one handover did.
enum Handed {
    /// The monitor took the report, on this model; the caller records that.
    Accepted(String),
    /// No model left to try: said once, and the report is left for a reader.
    GivenUp(String),
    /// A model was chosen and the report was written, but the session would not
    /// take it.
    Refused(String),
}

/// Puts `kind`'s session on a usable flash model, and gives it `report`.
///
/// # Invariant
///
/// One path for every kind: `commands::deliver` is how a turn starts anywhere, so
/// the queue gate, `@skill(...)` expansion, the tools and the window's rendering are
/// reused rather than re-implemented. `attempts` is the index of the candidate to
/// try, which is how a monitor that failed hands the same report to the next model.
async fn hand_over<F>(d: &Arc<Dispatch>, kind: Kind, attempts: u32, report: F) -> Handed
where
    F: FnOnce(&Entry) -> anyhow::Result<String>,
{
    let candidates = candidates();
    if candidates.is_empty() {
        tracing::warn!(
            session = %kind.session,
            "no usable flash model in the roster: the report is left for a reader"
        );
        return Handed::GivenUp(format!(
            "{}: a report is waiting, and no usable flash model is in the roster, \
             so no monitor turn was started",
            kind.session
        ));
    }
    let attempt = usize::try_from(attempts).unwrap_or(usize::MAX);
    let Some(entry) = candidates.get(attempt) else {
        // Every model has been tried for this report: say so once, and stop.
        tracing::error!(
            session = %kind.session,
            "the monitor failed on every usable flash model; leaving the report to a reader"
        );
        return Handed::GivenUp(format!(
            "{}: failed on every usable flash model ({}), so its report is left for a reader",
            kind.session,
            candidates.len()
        ));
    };
    let report = match report(entry) {
        Ok(report) => report,
        Err(e) => return Handed::Refused(format!("cannot write the report: {e}")),
    };
    if let Err(e) = adopt(d, kind, entry, attempts) {
        return Handed::Refused(format!(
            "cannot put {} on {}: {e}",
            kind.session, entry.model
        ));
    }
    // Stored after the handover: this attempt is one more than the ones recorded.
    let outcome = crate::commands::deliver(d, kind.session, report).await;
    if !outcome.ok {
        return Handed::Refused(outcome.message);
    }
    Handed::Accepted(entry.model.clone())
}

/// What the error report says: enough to start work without reading anything first.
fn report(
    d: &Dispatch,
    home: &Path,
    failure: &Failure,
    self_failed: bool,
    entry: &Entry,
) -> anyhow::Result<String> {
    let written = last_notice(d, failure.id)?;
    let prompts = crate::prompts::shared();
    if self_failed && let Some(again) = last_report(d, failure.id) {
        // The same report again: re-deriving it would lose what it was told.
        return Ok(prompts.render(
            crate::prompts::MONITOR_PROVIDERS_AGAIN,
            &[
                ("model", &failure.model),
                (
                    "failure",
                    &if written.is_empty() {
                        "(nothing)".to_string()
                    } else {
                        written.clone()
                    },
                ),
                ("again", &again),
            ],
        ));
    }
    // Only this failure varies; the map and the rules are the template's.
    let noted = if written.is_empty() {
        String::new()
    } else {
        format!("\nThe last thing the server recorded for it:\n\n{written}\n")
    };
    Ok(prompts.render(
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
            ("mine", &entry.model),
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
        format!("\nRead it back with:\n\n    gongga-cli get_messages --session {session} -40:-1\n")
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

/// Puts the monitor session on `entry`, creating it the first time.
///
/// # Invariant
///
/// The row is written before the turn reads its settings, and the level is the
/// one this model publishes, so a session is never left on a level its own model
/// refuses. Called before the report, because a turn that starts while the write
/// is pending would run on the model that just failed.
fn adopt(d: &Dispatch, kind: Kind, entry: &Entry, attempts: u32) -> anyhow::Result<()> {
    let fresh = d.store.get_session(kind.session)?.is_none();
    let session = d
        .sessions
        .ensure_on(kind.session, None, Some(&entry.model), None)?;
    if fresh {
        // Inherited, so its own failure continues the search: the model this
        // handover just tried is not the next candidate.
        d.store.set_monitor_attempts(session.id(), attempts)?;
    }
    let mut tuning = session.tuning();
    let level = entry.level_in_force(tuning.thinking_effort.as_deref());
    d.store.set_session_tuning(
        session.id(),
        Some(&entry.model),
        tuning.context_limit,
        level.as_deref(),
        &entry.model,
    )?;
    tuning.model = Some(entry.model.clone());
    tuning.thinking_effort = level;
    session.set_tuning(tuning);
    crate::commands::publish_sessions(d);
    Ok(())
}

/// The models a monitor turn may run on: usable flash, best allowance first.
///
/// # Invariant
///
/// Usable means the roster places it, a credential resolves for it, and it speaks
/// the Responses API -- the three ways a `set_session --model` would fail later.
/// A provider with no reading is not a refusal: it sorts after the ones that
/// reported, in roster order. See `docs/notes/server-monitor.md`.
pub fn candidates() -> Vec<Entry> {
    let catalog = crate::providers::roster();
    let mut scored: Vec<(Option<f64>, usize, Entry)> = Vec::new();
    let mut order = 0usize;
    for model in catalog.models() {
        let Ok(entry) = catalog.entry(&model) else {
            continue;
        };
        if entry.tier.as_deref() != Some("flash") {
            continue;
        }
        if !catalog.speaks_responses(&model) || catalog.resolve(&model).is_err() {
            continue;
        }
        scored.push((allowance(&entry.provider), order, entry));
        order += 1;
    }
    // Descending by what is left; an unmeasured provider keeps its roster place
    // behind every measured one.
    scored.sort_by(|a, b| match (a.0, b.0) {
        (Some(x), Some(y)) => y.total_cmp(&x).then(a.1.cmp(&b.1)),
        (Some(_), None) => std::cmp::Ordering::Less,
        (None, Some(_)) => std::cmp::Ordering::Greater,
        (None, None) => a.1.cmp(&b.1),
    });
    scored.into_iter().map(|(_, _, entry)| entry).collect()
}

/// The worst window one provider last reported, as a share of its ceiling.
///
/// # Invariant
///
/// The worst, not the average: a provider with a full month and an empty five-hour
/// window cannot serve the next turn. A wallet with no window is a nominal share,
/// so it competes instead of being shut out.
fn allowance(provider: &str) -> Option<f64> {
    let reading = crate::quota::of(provider)?;
    let windows: Vec<f64> = [
        reading.month_percent,
        reading.week_percent,
        reading.five_hour_percent,
    ]
    .into_iter()
    .flatten()
    .collect();
    if let Some(worst) = windows.iter().copied().reduce(f64::min) {
        return Some((worst / 100.0).clamp(0.0, 1.0));
    }
    match (reading.credit, reading.money) {
        (Some(credit), _) if credit > 0.0 => Some(0.5),
        (_, Some(money)) if money > 0.0 => Some(0.5),
        _ => None,
    }
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

/// The report this session was last given, which a retry sends again.
fn last_report(d: &Dispatch, id: i64) -> Option<String> {
    d.store.user_texts(id).ok()?.pop()
}
