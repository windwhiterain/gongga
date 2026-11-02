//! SPEC's monitor: a session told to fix what stopped with an error.
//!
//! See `docs/notes/server-monitor.md`.

use std::sync::Arc;

use protocol::LogLevel;

use crate::commands::Dispatch;
use crate::providers::Entry;
use crate::store::Failure;

/// The session SPEC gives failures to.
///
/// # Invariant
///
/// Built-in sessions are named with a leading `#`, and `set_session` refuses that
/// prefix: the namespace is reserved, so the harness never has to look at a
/// person's session and wonder whether it is its own.
pub const SESSION: &str = "#monitor";

/// Why `name` may not be a person's session, or `None` when it may.
///
/// # Invariant
///
/// Refused, not renamed: a session under a name the caller did not choose is a
/// session they will not find again. Deleting one is allowed -- a monitor that
/// cannot work has to be removable.
pub fn reserved(name: &str) -> Option<String> {
    name.starts_with('#').then(|| {
        format!(
            "names beginning with `#` belong to the harness ({SESSION} is one); \
             pick a name without it"
        )
    })
}

/// How many rows of the failing transcript a report opens with.
const CONTEXT_ROWS: i64 = 20;

/// Hands every unreported failure to the monitor session.
///
/// # Invariant
///
/// The store is the truth, not the bus: a turn that ended while this process was
/// not running is in `sessions.status`, and a subscriber that missed the event
/// would lose it. Called at start-up and on the tick that already wakes inboxes,
/// so the answer does not depend on which of the two happened first. See
/// `docs/notes/server-monitor.md`.
pub async fn sweep(d: &Arc<Dispatch>, home: &std::path::Path) {
    if d.shutting_down.load(std::sync::atomic::Ordering::SeqCst) || d.shutdown.is_cancelled() {
        return;
    }
    let Ok(failures) = d.store.failures() else {
        return;
    };
    for failure in failures {
        // Reported already: the handed-over row is the whole record of that.
        if failure.last_seq <= failure.monitor_seq {
            continue;
        }
        dispatch(d, home, &failure, failure.name == SESSION).await;
    }
}

/// One failure: choose a model for the monitor, and give it the report.
async fn dispatch(d: &Arc<Dispatch>, home: &std::path::Path, failure: &Failure, self_failed: bool) {
    let candidates = candidates();
    if candidates.is_empty() {
        tracing::warn!(
            session = %failure.name,
            "no usable flash model in the roster: the failure is left for a reader"
        );
        d.bus.publish(protocol::Event::Log {
            level: LogLevel::Error,
            message: format!(
                "{}: stopped with an error, and no usable flash model is in the roster, \
                 so no monitor turn was started",
                failure.name
            ),
        });
        // Recorded as delivered so the search does not run again every tick.
        let _ = d
            .store
            .mark_monitored(failure.id, failure.last_seq, failure.monitor_attempts);
        return;
    }
    // The attempts made so far are also the index of the next candidate.
    let attempt = usize::try_from(failure.monitor_attempts).unwrap_or(usize::MAX);
    let entry = match candidates.get(attempt) {
        Some(entry) => entry,
        None if !self_failed && failure.monitor_attempts == 0 => &candidates[0],
        None => {
            // Every model has been tried for this failure: say so once, and stop.
            tracing::error!(
                session = %failure.name,
                "the monitor failed on every usable flash model; leaving the failure to a reader"
            );
            d.bus.publish(protocol::Event::Log {
                level: LogLevel::Error,
                message: format!(
                    "monitor: failed on every usable flash model ({}), so {} is left for a reader",
                    candidates.len(),
                    failure.name
                ),
            });
            let _ = d
                .store
                .mark_monitored(failure.id, failure.last_seq, failure.monitor_attempts);
            return;
        }
    };
    // Stored after the handover: this attempt is one more than the ones recorded.
    let attempts = failure.monitor_attempts.saturating_add(1);

    let report = match report(d, home, failure, self_failed, entry) {
        Ok(report) => report,
        Err(e) => {
            tracing::warn!(session = %failure.name, error = %e, "cannot write the monitor report");
            return;
        }
    };
    if let Err(e) = adopt(d, entry, attempts) {
        tracing::warn!(error = %e, "cannot put the monitor session on {}", entry.model);
        return;
    }
    let outcome = crate::commands::deliver(d, SESSION, report).await;
    if !outcome.ok {
        tracing::warn!(
            session = %failure.name,
            why = %outcome.message,
            "the monitor session did not take the report"
        );
        return;
    }
    tracing::info!(
        session = %failure.name,
        model = entry.model,
        attempt = attempts,
        "handed a failure to the monitor"
    );
    // After, not before: a crash between the two repeats a report, which is
    // recoverable, where the other order loses one.
    let _ = d
        .store
        .mark_monitored(failure.id, failure.last_seq, attempts);
}

/// What the report says: enough to start work without reading anything first.
fn report(
    d: &Dispatch,
    home: &std::path::Path,
    failure: &Failure,
    self_failed: bool,
    entry: &Entry,
) -> anyhow::Result<String> {
    let written = last_notice(d, failure.id)?;
    if self_failed && let Some(again) = last_report(d, failure.id) {
        // The same report again: re-deriving it would lose what it was told.
        return Ok(format!(
            "The monitor failed on {model}. This is the report it was given, unchanged; \
             the failure is in this conversation.\n\n\
             What the server recorded for that turn:\n\n{failure_note}\n\n---\n\n{again}",
            model = failure.model,
            failure_note = if written.is_empty() {
                "(nothing)".to_string()
            } else {
                written.clone()
            },
        ));
    }
    let home = home.display();
    let cwd = std::env::current_dir()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_default();
    let mut out = String::new();
    out.push_str(
        "A gongga session stopped with an error. Read it, find the harness-side cause, \
         and fix it. This session is the monitor: work here, with shells and with \
         `gongga-cli`, exactly as a person would.\n\n",
    );
    out.push_str(&format!(
        "- session: {name}\n\
         - its model: {model}\n\
         - rows {from}..{to} of its transcript\n\
         - harness build: {build}\n\
         - home: {home}\n\
         - server: pid {pid}, cwd {cwd}\n\
         - this turn runs on: {mine}\n",
        name = failure.name,
        model = failure.model,
        from = (failure.last_seq - CONTEXT_ROWS).max(0),
        to = failure.last_seq,
        build = protocol::version_line(),
        pid = std::process::id(),
        mine = entry.model,
    ));
    if !written.is_empty() {
        out.push_str(&format!(
            "\nThe last thing the server recorded for it:\n\n{written}\n"
        ));
    }
    out.push_str(&format!(
        "\nRead it back with:\n\n    gongga-cli get_messages --session {name} --range -40:-1\n",
        name = failure.name,
    ));
    out.push_str(
        "\nIf the cause is a harness bug, fix the source and restart the server \
         (`@skill: self-control`); if it is a credential or a quota, put that session on \
         a model the roster can serve (`gongga-cli set_session`). Report what you found \
         and what you changed in one message when you are done.\n",
    );
    Ok(out)
}

/// Puts the monitor session on `entry`, creating it the first time.
///
/// # Invariant
///
/// The row is written before the turn reads its settings, and the level is the
/// one this model publishes, so a session is never left on a level its own model
/// refuses. Called before the report, because a turn that starts while the write
/// is pending would run on the model that just failed.
fn adopt(d: &Dispatch, entry: &Entry, attempts: u32) -> anyhow::Result<()> {
    let fresh = d.store.get_session(SESSION)?.is_none();
    let session = d
        .sessions
        .ensure_on(SESSION, None, Some(&entry.model), None)?;
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
