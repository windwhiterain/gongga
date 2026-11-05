//! What a tool says it is: the code's default, as a config may word it otherwise.
//!
//! See `docs/notes/server-tools-text.md`.

use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::SystemTime;

use serde_json::Value;

/// The one section this module reads.
///
/// # Invariant
///
/// Not `[tools]`: that one is the runtime settings of a tool (`[tools.bash]`), and
/// a table of settings must not be read as a table of sentences.
const TABLE: &str = "tool_text";

/// One tool's words: what it is, and what each named argument is for.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
struct Entry {
    description: Option<String>,
    arguments: BTreeMap<String, String>,
}

/// What was on disk when the file was last read: enough to notice an edit.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Stamp {
    size: u64,
    modified: Option<SystemTime>,
}

impl Stamp {
    fn of(path: &Path) -> Option<Self> {
        let meta = std::fs::metadata(path).ok()?;
        Some(Self {
            size: meta.len(),
            modified: meta.modified().ok(),
        })
    }
}

#[derive(Default)]
struct Snapshot {
    stamp: Option<Stamp>,
    entries: BTreeMap<String, Entry>,
    complained: bool,
}

/// The words in force: the home's `[tools]` over the pinned content's.
pub struct Texts {
    path: Option<PathBuf>,
    pinned: BTreeMap<String, Entry>,
    snapshot: Mutex<Snapshot>,
    /// Names already reported, so a misspelling is said once and not every step.
    warned: Mutex<HashSet<String>>,
}

impl Texts {
    fn layered(path: Option<PathBuf>, pinned: BTreeMap<String, Entry>) -> Self {
        Self {
            path,
            pinned,
            snapshot: Mutex::new(Snapshot::default()),
            warned: Mutex::new(HashSet::new()),
        }
    }

    /// The tools whose words differ from the pinned content: a home that never
    /// mentions a tool overrides none.
    pub fn overridden(&self) -> Vec<String> {
        let mut names: Vec<String> = self
            .entries()
            .into_iter()
            .filter(|(name, entry)| self.pinned.get(name) != Some(entry))
            .map(|(name, _)| name)
            .collect();
        names.sort();
        names
    }

    /// Every tool's words as they stand now, cheap enough to ask once a step.
    fn entries(&self) -> BTreeMap<String, Entry> {
        let Some(path) = &self.path else {
            return self.pinned.clone();
        };
        let stamp = Stamp::of(path);
        let mut snapshot = self.snapshot.lock().unwrap_or_else(|e| e.into_inner());
        if snapshot.stamp == stamp && stamp.is_some() {
            return merged(&self.pinned, &snapshot.entries);
        }
        let (entries, problems) = match std::fs::read_to_string(path) {
            Ok(text) => read_table(&text),
            Err(e) => {
                tracing::warn!(
                    file = %path.display(),
                    error = %e,
                    "cannot read the tool words; the code's own stand"
                );
                (BTreeMap::new(), Vec::new())
            }
        };
        if problems.is_empty() || !snapshot.complained {
            for problem in &problems {
                tracing::warn!(file = %path.display(), problem = %problem, "tool words ignored");
            }
            snapshot.complained = !problems.is_empty();
        }
        snapshot.stamp = stamp;
        snapshot.entries = entries;
        merged(&self.pinned, &snapshot.entries)
    }

    /// What one tool says it is: the config's words, or `default` as written.
    pub fn description(&self, name: &str, default: &str) -> String {
        self.entries()
            .get(name)
            .and_then(|entry| entry.description.clone())
            .unwrap_or_else(|| default.to_string())
    }

    /// Rewrites the argument sentences of one schema, leaving its shape alone.
    ///
    /// # Invariant
    ///
    /// Only a description is written: the property stays the object it was, so a
    /// config cannot add an argument the tool does not read. `docs/notes/server-tools-mod.md`.
    pub fn apply(&self, name: &str, schema: &mut Value) {
        let Some(entry) = self.entries().remove(name) else {
            return;
        };
        if entry.arguments.is_empty() {
            return;
        }
        let Some(properties) = schema.get_mut("properties").and_then(Value::as_object_mut) else {
            return;
        };
        for (argument, text) in entry.arguments {
            match properties.get_mut(&argument) {
                Some(property) if property.is_object() => {
                    property["description"] = Value::String(text);
                }
                _ => self.warn_once(format!("{name}.{argument}"), || {
                    format!("`{name}` takes no argument {argument:?}, so its words are ignored")
                }),
            }
        }
    }

    fn warn_once(&self, key: String, message: impl FnOnce() -> String) {
        let mut warned = self.warned.lock().unwrap_or_else(|e| e.into_inner());
        if warned.insert(key) {
            tracing::warn!("{}", message());
        }
    }
}

/// The home's words over the pinned ones, field by field.
///
/// # Invariant
///
/// A home that overrides one argument keeps the shipped sentence on the others:
/// the merge is per field, never per tool, so an edit cannot silently delete the
/// rest of a tool's prose.
fn merged(
    pinned: &BTreeMap<String, Entry>,
    home: &BTreeMap<String, Entry>,
) -> BTreeMap<String, Entry> {
    let mut out = pinned.clone();
    for (name, entry) in home {
        let target = out.entry(name.clone()).or_default();
        if entry.description.is_some() {
            target.description = entry.description.clone();
        }
        for (argument, text) in &entry.arguments {
            target.arguments.insert(argument.clone(), text.clone());
        }
    }
    out
}

/// The process's words, installed at startup from the home's config.
static SHARED: OnceLock<Texts> = OnceLock::new();

/// Points the process at one home's config and reads the shipped words behind it.
pub fn install(config: &Path) {
    let pinned = match crate::ship::config_text() {
        Some(text) => read_table(&text).0,
        None => {
            tracing::warn!(
                content = crate::ship::resolved_commit().unwrap_or("nothing resolved"),
                protocol = crate::ship::link().protocol,
                "the shipped config is not readable here; the code's own words stand"
            );
            BTreeMap::new()
        }
    };
    let _ = SHARED.set(Texts::layered(Some(config.to_path_buf()), pinned));
    let texts = shared();
    let overridden = texts.overridden();
    if !overridden.is_empty() {
        tracing::info!(
            file = %config.display(),
            overridden = ?overridden,
            "tool words: the code's own, plus what this home reworded"
        );
    }
}

/// The words every request is built from; the code's own until `install` runs.
pub fn shared() -> &'static Texts {
    SHARED.get_or_init(|| Texts::layered(None, BTreeMap::new()))
}

/// The `[tools]` of a TOML text, with whatever was wrong with it.
///
/// # Safety
///
/// A file that does not parse yields nothing: the code's own words are the answer
/// to a broken config, not a server that cannot start.
fn read_table(text: &str) -> (BTreeMap<String, Entry>, Vec<String>) {
    let mut out = BTreeMap::new();
    let mut problems = Vec::new();
    let parsed = match text.parse::<toml::Table>() {
        Ok(table) => table,
        Err(e) => {
            problems.push(format!("cannot parse as TOML: {e}"));
            return (out, problems);
        }
    };
    let Some(value) = parsed.get(TABLE) else {
        return (out, problems);
    };
    let Some(tools) = value.as_table() else {
        problems.push(format!("[{TABLE}] is not a table"));
        return (out, problems);
    };
    for (name, value) in tools {
        let Some(table) = value.as_table() else {
            problems.push(format!("`{TABLE}.{name}` is not a table, so it is ignored"));
            continue;
        };
        let mut entry = Entry::default();
        for (key, value) in table {
            match key.as_str() {
                "description" => match value.as_str() {
                    Some(text) => entry.description = Some(text.to_string()),
                    None => problems.push(format!("`{TABLE}.{name}.description` is not a string")),
                },
                "arguments" => {
                    let Some(arguments) = value.as_table() else {
                        problems.push(format!("`{TABLE}.{name}.arguments` is not a table"));
                        continue;
                    };
                    for (argument, text) in arguments {
                        match text.as_str() {
                            Some(text) => {
                                entry.arguments.insert(argument.clone(), text.to_string());
                            }
                            None => problems.push(format!(
                                "`{TABLE}.{name}.arguments.{argument}` is not a string"
                            )),
                        }
                    }
                }
                other => problems.push(format!(
                    "`{TABLE}.{name}.{other}` is not a key a tool has; it is ignored"
                )),
            }
        }
        out.insert(name.clone(), entry);
    }
    (out, problems)
}
