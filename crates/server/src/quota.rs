//! SPEC's `active_quota` row: what the provider a step ran on still allows.
//!
//! See `docs/notes/server-quota.md`.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use protocol::Quota;
use serde_json::Value;

use crate::bus::EventBus;

/// How long one provider's own reader may take before the row says it timed out.
///
/// # Invariant
///
/// A hang guard, not a budget: the script is bounded so a dead endpoint cannot
/// keep a child process alive for the life of the server.
const READER_TIMEOUT: Duration = Duration::from_secs(10);

/// The newest reading, per provider, and whether one is being read.
struct Last {
    /// The newest reading of any provider: SPEC's `active_quota` row.
    reading: Option<Quota>,
    /// Every provider's newest reading, by roster id.
    ///
    /// # Invariant
    ///
    /// Kept per provider because a choice between providers -- which flash model a
    /// monitor turn runs on -- is a comparison, and the single newest reading says
    /// nothing about the others. In memory only: a reading is a measurement of a
    /// moment, not a fact about a home.
    by_provider: std::collections::HashMap<String, Quota>,
    /// One read at a time: a step end inside a slow read starts no second child.
    reading_now: AtomicBool,
    /// Invariant: one wake per distinct reason, never one per step.
    /// See `docs/notes/server-monitor.md`.
    announced: std::collections::HashMap<String, String>,
}

fn state() -> &'static Mutex<Last> {
    static LAST: OnceLock<Mutex<Last>> = OnceLock::new();
    LAST.get_or_init(|| {
        Mutex::new(Last {
            reading: None,
            by_provider: std::collections::HashMap::new(),
            reading_now: AtomicBool::new(false),
            announced: std::collections::HashMap::new(),
        })
    })
}

/// The newest reading, for a window that has just attached.
pub fn last() -> Option<Quota> {
    state().lock().ok()?.reading.clone()
}

/// What a provider last reported, if it was ever read in this run.
pub fn of(provider: &str) -> Option<Quota> {
    state().lock().ok()?.by_provider.get(provider).cloned()
}

/// Reads `provider`'s allowance and publishes it, at a step's end.
///
/// # Invariant
///
/// Never on the turn's path: the read is a network call through a child process,
/// so it is spawned and the step goes on. See `docs/notes/server-quota.md`.
/// The event a provider's unreadable allowance is logged as, for the monitor.
pub const EVENT: &str = "quota";

pub fn publish(bus: EventBus, store: crate::store::Store, provider: Option<String>) {
    let Some(provider) = provider else {
        return;
    };
    {
        let Ok(guard) = state().lock() else {
            return;
        };
        if guard.reading_now.swap(true, Ordering::SeqCst) {
            return;
        }
    }
    tokio::spawn(async move {
        let reading = read(&provider).await;
        // A note with no numbers: see `docs/notes/server-monitor.md`.
        if let Some(failed) = &reading
            && let Some(note) = &failed.note
            && failed.month_percent.is_none()
            && failed.week_percent.is_none()
            && failed.five_hour_percent.is_none()
            && failed.credit.is_none()
            && failed.money.is_none()
        {
            let fresh = match state().lock() {
                Ok(mut state) => {
                    let fresh = state.announced.get(&provider) != Some(note);
                    if fresh {
                        state.announced.insert(provider.clone(), note.clone());
                    }
                    fresh
                }
                Err(_) => false,
            };
            if fresh {
                let _ = store.append_event_row(
                    None,
                    EVENT,
                    &serde_json::json!({"provider": provider, "note": note}),
                );
            }
        }
        if let Ok(mut state) = state().lock() {
            state.reading_now.store(false, Ordering::SeqCst);
            if let Some(reading) = &reading {
                state.reading = Some(reading.clone());
                state
                    .by_provider
                    .insert(reading.provider.clone(), reading.clone());
            }
        }
        if let Some(reading) = reading {
            bus.publish(protocol::Event::Quota(reading));
        }
    });
}

/// One provider's allowance, or `None` when it publishes no reader at all.
pub async fn read(provider: &str) -> Option<Quota> {
    read_in(&crate::providers::roster(), provider, interpreter()).await
}

/// The same, against a roster the caller hands in: what a test drives.
pub async fn read_in(
    catalog: &crate::providers::Catalog,
    provider: &str,
    program: Option<PathBuf>,
) -> Option<Quota> {
    let access = catalog.quota_of(provider)?;
    run(&access, program).await
}

/// The interpreter, found once: every candidate probe is a child process.
fn interpreter() -> Option<PathBuf> {
    static PROGRAM: OnceLock<Option<PathBuf>> = OnceLock::new();
    PROGRAM.get_or_init(crate::config::detect_python).clone()
}

/// Tries each query in the roster's order and returns the first that reads.
///
/// # Invariant
///
/// File order is the preference order: the first reading that answers is the
/// one shown, and a query that fails is reported only when every one did. A
/// provider whose queries name no script at all is `None` -- it has no allowance
/// to show, and a row saying so at every step is noise.
pub async fn run(
    access: &crate::providers::QuotaAccess,
    program: Option<PathBuf>,
) -> Option<Quota> {
    let readable: Vec<&crate::providers::QuotaQuery> = access
        .queries
        .iter()
        .filter(|q| q.script.is_some())
        .collect();
    if readable.is_empty() {
        return None;
    }
    let Some(program) = program.as_ref() else {
        return Some(Quota {
            provider: access.provider.clone(),
            note: Some("no Python to run the quota script with".to_string()),
            ..Quota::default()
        });
    };
    let mut failure: Option<String> = None;
    for query in readable {
        let script = query.script.as_ref()?;
        match ask(program, script, query, access.key.as_deref()).await {
            Ok(text) => match parse(&text) {
                Ok(reading) => return Some(reading.of(&access.provider, &query.unit)),
                Err(e) => failure = Some(format!("{}: {e}", query.id)),
            },
            Err(e) => failure = Some(format!("{}: {e}", query.id)),
        }
    }
    Some(Quota {
        provider: access.provider.clone(),
        note: failure,
        ..Quota::default()
    })
}

/// Runs one reader and returns the line it printed.
async fn ask(
    program: &PathBuf,
    script: &PathBuf,
    query: &crate::providers::QuotaQuery,
    key: Option<&str>,
) -> Result<String, String> {
    let mut command = tokio::process::Command::new(program);
    command.arg(script).kill_on_drop(true);
    // The key goes in the environment, which is where the script looks: argv is
    // readable by every process on the machine.
    if let (Some(name), Some(key)) = (&query.script_env, key) {
        command.env(name, key);
    }
    command.stdin(std::process::Stdio::null());
    let child = command.output();
    let out = match tokio::time::timeout(READER_TIMEOUT, child).await {
        Ok(Ok(out)) => out,
        Ok(Err(e)) => return Err(format!("cannot run the reader: {e}")),
        Err(_) => return Err("the reader did not answer in time".to_string()),
    };
    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    if !out.status.success() {
        let said = String::from_utf8_lossy(&out.stderr).trim().to_string();
        return Err(if said.is_empty() {
            format!("the reader exited {}", out.status)
        } else {
            said
        });
    }
    Ok(stdout)
}

/// One reading as the roster's scripts print it.
///
/// # Invariant
///
/// Kept as the script's own numbers and names: which window is "month" and which
/// is "5h" is the mapping's business, never the reading's.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Reading {
    pub remaining: Option<f64>,
    pub windows: Vec<Window>,
    pub detail: Option<String>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Window {
    pub name: String,
    pub remaining: f64,
    /// The window's size, when the provider states one; unscored otherwise.
    pub capacity: Option<f64>,
}

/// Reads the JSON a reader printed.
///
/// # Invariant
///
/// The line a reader ends with is the reading, so anything it said before it --
/// a warning, a progress note -- is not the answer. The whole text is tried first,
/// because a reading pretty-printed over several lines is still one reading.
pub fn parse(text: &str) -> Result<Reading, String> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Err("the reader printed nothing".to_string());
    }
    let line = trimmed.lines().next_back().unwrap_or(trimmed);
    let value: Value = serde_json::from_str(trimmed)
        .or_else(|_| serde_json::from_str(line))
        .map_err(|e| format!("the reading is not JSON: {e}"))?;
    let number = |v: Option<&Value>| v.and_then(Value::as_f64).filter(|n| n.is_finite());
    let windows = value
        .get("windows")
        .and_then(Value::as_array)
        .map(|list| {
            list.iter()
                .filter_map(|w| {
                    Some(Window {
                        name: w.get("name")?.as_str()?.to_string(),
                        remaining: number(w.get("remaining"))?,
                        capacity: number(w.get("capacity")),
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    Ok(Reading {
        remaining: number(value.get("remaining")),
        windows,
        detail: value
            .get("detail")
            .and_then(Value::as_str)
            .map(str::to_string),
        error: value
            .get("error")
            .and_then(Value::as_str)
            .map(str::to_string),
    })
}

/// Which of SPEC's three windows a provider's own name means.
///
/// # Invariant
///
/// An unknown name is not guessed at: a provider inventing a fourth period leaves
/// that number out rather than filing it under a window it is not.
pub fn window_of(name: &str) -> Option<WindowName> {
    let name = name.to_ascii_lowercase();
    if name.contains("month") {
        return Some(WindowName::Month);
    }
    if name.contains("week") || name.contains("7d") || name.contains("seven") {
        return Some(WindowName::Week);
    }
    if name.contains("5h")
        || name.contains("five")
        || name.contains("hour")
        || name.contains("current")
        || name.contains("rolling")
        || name.contains("session")
    {
        return Some(WindowName::FiveHour);
    }
    None
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WindowName {
    Month,
    Week,
    FiveHour,
}

impl Reading {
    /// The reading as SPEC's row spells it, with `unit` saying what the numbers are.
    pub fn of(&self, provider: &str, unit: &str) -> Quota {
        let mut quota = Quota {
            provider: provider.to_string(),
            currency: currency_of(unit).unwrap_or("¥").to_string(),
            note: self.error.clone(),
            ..Quota::default()
        };
        for window in &self.windows {
            let share = match window.capacity {
                // A stated ceiling is a share of it; SPEC's `n%`.
                Some(capacity) if capacity > 0.0 => {
                    Some((window.remaining / capacity * 100.0).clamp(0.0, 100.0))
                }
                // Stated without one: a count of what is left, which is `credit n`.
                _ => {
                    quota.credit.get_or_insert(window.remaining);
                    continue;
                }
            };
            match window_of(&window.name) {
                Some(WindowName::Month) => quota.month_percent = share,
                Some(WindowName::Week) => quota.week_percent = share,
                Some(WindowName::FiveHour) => quota.five_hour_percent = share,
                None => {}
            }
        }
        // A wallet has no windows, so its number is the balance: a plan's shares
        // already say which window is tight.
        if self.windows.is_empty() && currency_of(unit).is_some() {
            quota.money = self.remaining;
        }
        quota
    }
}

/// The symbol of a `<currency>-left` unit, which is what makes a reading money.
fn currency_of(unit: &str) -> Option<&'static str> {
    match unit.split('-').next()? {
        "cny" | "rmb" => Some("¥"),
        "usd" => Some("$"),
        _ => None,
    }
}
