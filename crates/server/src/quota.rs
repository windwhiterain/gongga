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

/// The reading a window that attaches now should see, and whether one is being read.
struct Last {
    reading: Option<Quota>,
    /// One read at a time: a step end inside a slow read starts no second child.
    reading_now: AtomicBool,
}

fn state() -> &'static Mutex<Last> {
    static LAST: OnceLock<Mutex<Last>> = OnceLock::new();
    LAST.get_or_init(|| {
        Mutex::new(Last {
            reading: None,
            reading_now: AtomicBool::new(false),
        })
    })
}

/// The newest reading, for a window that has just attached.
pub fn last() -> Option<Quota> {
    state().lock().ok()?.reading.clone()
}

/// Reads `provider`'s allowance and publishes it, at a step's end.
///
/// # Invariant
///
/// Never on the turn's path: the read is a network call through a child process,
/// so it is spawned and the step goes on. See `docs/notes/server-quota.md`.
pub fn publish(bus: EventBus, provider: Option<String>) {
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
        if let Ok(mut state) = state().lock() {
            state.reading_now.store(false, Ordering::SeqCst);
            if let Some(reading) = &reading {
                state.reading = Some(reading.clone());
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

#[cfg(test)]
mod tests {
    use super::*;

    fn window(name: &str, remaining: f64, capacity: Option<f64>) -> Window {
        Window {
            name: name.to_string(),
            remaining,
            capacity,
        }
    }

    /// A wallet: one number, in the currency the roster's unit names.
    #[test]
    fn a_wallet_reading_becomes_money() {
        let text = r#"{"remaining":215.52,"detail":"215.52 CNY"}"#;
        let quota = parse(text).unwrap().of("deepseek-official", "cny-left");
        assert_eq!(quota.money, Some(215.52));
        assert_eq!(quota.currency, "¥");
        assert_eq!(quota.month_percent, None);
        assert_eq!(quota.note, None);
    }

    /// A plan with three metered windows, as SPEC names them.
    #[test]
    fn three_windows_become_the_three_shares_spec_names() {
        let text = r#"{"remaining":42,"windows":[
            {"name":"monthly","remaining":75,"capacity":100},
            {"name":"weekly","remaining":42,"capacity":100},
            {"name":"five-hour","remaining":90,"capacity":100}]}"#;
        let quota = parse(text).unwrap().of("qwen", "percent-left");
        assert_eq!(quota.month_percent, Some(75.0));
        assert_eq!(quota.week_percent, Some(42.0));
        assert_eq!(quota.five_hour_percent, Some(90.0));
        assert_eq!(
            quota.money, None,
            "a plan spends its windows; the wallet is not the same fact"
        );
    }

    /// A window stated without a ceiling is a count, which is SPEC's `credit`.
    #[test]
    fn an_unscored_window_is_a_credit() {
        let text = r#"{"remaining":14,"windows":[
            {"name":"monthly","remaining":37.5},
            {"name":"weekly","remaining":14,"capacity":35}]}"#;
        let quota = parse(text).unwrap().of("command-code-goat", "usd-left");
        assert_eq!(quota.credit, Some(37.5), "the credit pool, as reported");
        assert_eq!(quota.week_percent, Some(40.0));
        assert_eq!(quota.month_percent, None, "unscored, so no share");
        assert_eq!(quota.currency, "$");
        assert_eq!(quota.money, None);
    }

    /// A provider's own name for a period is mapped, and an unknown one is not
    /// filed under a window it is not.
    #[test]
    fn a_window_name_is_mapped_or_left_out() {
        assert_eq!(window_of("monthly"), Some(WindowName::Month));
        assert_eq!(window_of("month"), Some(WindowName::Month));
        assert_eq!(window_of("weekly"), Some(WindowName::Week));
        assert_eq!(window_of("7d"), Some(WindowName::Week));
        assert_eq!(window_of("five-hour"), Some(WindowName::FiveHour));
        assert_eq!(window_of("current"), Some(WindowName::FiveHour));
        assert_eq!(window_of("5h"), Some(WindowName::FiveHour));
        assert_eq!(window_of("per-minute"), None);

        let text =
            r#"{"remaining":50,"windows":[{"name":"per-minute","remaining":50,"capacity":100}]}"#;
        let quota = parse(text).unwrap().of("x", "percent-left");
        assert_eq!(
            (
                quota.month_percent,
                quota.week_percent,
                quota.five_hour_percent
            ),
            (None, None, None)
        );
    }

    /// The reading's complaint travels with it, even when the numbers are there.
    #[test]
    fn a_flagged_reading_keeps_its_words() {
        let text = r#"{"remaining":0,"error":"the weekly window is exhausted"}"#;
        let quota = parse(text).unwrap().of("command-code-goat", "usd-left");
        assert_eq!(
            quota.note.as_deref(),
            Some("the weekly window is exhausted")
        );
        assert_eq!(quota.money, Some(0.0));
    }

    /// A reader is a child process with the key in its environment, and what it
    /// prints is the row.
    ///
    /// # Invariant
    ///
    /// The key never reaches argv: the script reads the variable its roster entry
    /// names, exactly as the roster's own scripts do.
    #[tokio::test]
    async fn a_reader_script_is_run_with_the_key_in_its_environment() {
        let Some(python) = crate::config::detect_python() else {
            return;
        };
        let dir = tempfile::tempdir().unwrap();
        let script = dir.path().join("reader.py");
        std::fs::write(
            &script,
            "import os, sys\n\
             key = os.environ.get('P_KEY', '')\n\
             print('{\"remaining\": ' + ('42' if key == 'sk-env' else '0') + ', \"windows\": [{\"name\": \"weekly\", \"remaining\": 42, \"capacity\": 100}]}')\n",
        )
        .unwrap();
        let access = crate::providers::QuotaAccess {
            provider: "p".to_string(),
            queries: vec![crate::providers::QuotaQuery {
                id: "wallet".to_string(),
                unit: "percent-left".to_string(),
                script: Some(script.clone()),
                script_env: Some("P_KEY".to_string()),
            }],
            key: Some("sk-env".to_string()),
        };
        let quota = run(&access, Some(python)).await.expect("a reading");
        assert_eq!(quota.provider, "p");
        assert_eq!(quota.week_percent, Some(42.0), "{quota:?}");
        assert_eq!(quota.note, None);

        // A reader that fails is a note, and the next query is tried.
        let broken = crate::providers::QuotaAccess {
            provider: "p".to_string(),
            queries: vec![
                crate::providers::QuotaQuery {
                    id: "broken".to_string(),
                    unit: "percent-left".to_string(),
                    script: Some(dir.path().join("missing.py")),
                    script_env: None,
                },
                access.queries[0].clone(),
            ],
            key: Some("sk-env".to_string()),
        };
        let quota = run(&broken, crate::config::detect_python())
            .await
            .expect("the second reader answered");
        assert_eq!(quota.week_percent, Some(42.0), "{quota:?}");
    }

    /// A roster file, its script and the row it produces: the whole path SPEC's
    /// `active_quota` row is read through.
    #[tokio::test]
    async fn a_roster_entry_is_read_through_its_own_script() {
        let Some(python) = crate::config::detect_python() else {
            return;
        };
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("providers")).unwrap();
        std::fs::write(
            dir.path().join("providers/demo.py"),
            "print('{\"remaining\": 42, \"windows\": [{\"name\": \"five-hour\", \"remaining\": 90, \"capacity\": 100}, {\"name\": \"weekly\", \"remaining\": 40, \"capacity\": 100}], \"detail\": \"ok\"}')\n",
        )
        .unwrap();
        std::fs::write(
            dir.path().join("providers.toml"),
            r#"[[provider]]
id = "demo"
[[provider.model]]
id = "m"
[[provider.quota_query]]
id = "plan"
unit = "percent-left"
script = "providers/demo.py"
script_env = "DEMO_KEY"
"#,
        )
        .unwrap();
        let catalog = crate::providers::Catalog::load(&dir.path().join("providers.toml"));
        let quota = read_in(&catalog, "demo", Some(python))
            .await
            .expect("a reading");
        assert_eq!(quota.provider, "demo");
        assert_eq!(quota.five_hour_percent, Some(90.0));
        assert_eq!(quota.week_percent, Some(40.0));
        assert_eq!(quota.month_percent, None);
        assert_eq!(quota.note, None);
    }

    /// A provider with no readable query has no row: nothing to say about it.
    #[tokio::test]
    async fn a_provider_with_no_reader_has_no_row() {
        let none = crate::providers::QuotaAccess {
            provider: "qwen-dashscope".to_string(),
            queries: vec![crate::providers::QuotaQuery {
                id: "none-by-api-key".to_string(),
                unit: String::new(),
                script: None,
                script_env: None,
            }],
            key: None,
        };
        assert_eq!(run(&none, None).await, None);
        let empty = crate::providers::QuotaAccess {
            provider: "p".to_string(),
            queries: Vec::new(),
            key: None,
        };
        assert_eq!(run(&empty, None).await, None);
    }

    /// A reader that cannot run is a row saying why, not a silent absence.
    #[tokio::test]
    async fn a_reader_with_no_interpreter_says_so() {
        let access = crate::providers::QuotaAccess {
            provider: "p".to_string(),
            queries: vec![crate::providers::QuotaQuery {
                id: "plan".to_string(),
                unit: "percent-left".to_string(),
                script: Some(std::path::PathBuf::from("providers/p.py")),
                script_env: None,
            }],
            key: None,
        };
        let quota = run(&access, None).await.expect("a row");
        assert_eq!(
            quota.note.as_deref(),
            Some("no Python to run the quota script with")
        );
    }

    /// Nothing printed, or a line that is not JSON, is a reason rather than a zero.
    #[test]
    fn a_reader_that_says_nothing_is_not_a_reading() {
        assert!(parse("").unwrap_err().contains("printed nothing"));
        assert!(
            parse("no credential: set X\n")
                .unwrap_err()
                .contains("not JSON")
        );
        // stderr-style chatter before the reading does not hide it.
        let reading = parse("warming up\n{\"remaining\":1}\n").unwrap();
        assert_eq!(reading.remaining, Some(1.0));
    }
}
