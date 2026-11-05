//! What each tool says it is: one config, and no default anywhere else.
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

/// The entry a tool's shared arguments come from, in the tunnel's own words.
const GENERAL: &str = "general";

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

/// The words in force: read from one config and from nowhere else.
pub struct Texts {
    path: Option<PathBuf>,
    snapshot: Mutex<Snapshot>,
    /// Names already reported, so a misspelling is said once and not every step.
    warned: Mutex<HashSet<String>>,
}

impl Texts {
    fn at(path: Option<PathBuf>) -> Self {
        Self {
            path,
            snapshot: Mutex::new(Snapshot::default()),
            warned: Mutex::new(HashSet::new()),
        }
    }

    /// Every tool's words as they stand now, cheap enough to ask once a step.
    fn entries(&self) -> BTreeMap<String, Entry> {
        let Some(path) = &self.path else {
            return BTreeMap::new();
        };
        let stamp = Stamp::of(path);
        let mut snapshot = self.snapshot.lock().unwrap_or_else(|e| e.into_inner());
        if snapshot.stamp == stamp && stamp.is_some() {
            return snapshot.entries.clone();
        }
        let (entries, problems) = match std::fs::read_to_string(path) {
            Ok(text) => read_table(&text),
            Err(e) => {
                tracing::warn!(
                    file = %path.display(),
                    error = %e,
                    "cannot read the tool words; no tool can say what it is"
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
        snapshot.entries.clone()
    }

    /// What one tool says it is.
    pub fn description(&self, tool: &str) -> Option<String> {
        self.entries()
            .get(tool)
            .and_then(|entry| entry.description.clone())
    }

    /// Rewrites the argument sentences of one schema, leaving its shape alone.
    ///
    /// # Invariant
    ///
    /// Only a `description` is written, and only into a property the schema already
    /// declares: a config cannot add an argument the tool does not read.
    pub fn apply(&self, tool: &str, schema: &mut Value) {
        let entries = self.entries();
        let Some(properties) = schema.get_mut("properties").and_then(Value::as_object_mut) else {
            return;
        };
        for (argument, property) in properties.iter_mut() {
            if !property.is_object() {
                continue;
            }
            let Some(text) = sentence(&entries, tool, argument) else {
                continue;
            };
            property["description"] = Value::String(text);
        }
    }

    /// What the registry says that this config does not word: a home that cannot start.
    ///
    /// # Invariant
    ///
    /// Both halves are required -- the tool's own sentence, and one per argument its
    /// schema declares -- because half a schema is a model guessing what an argument
    /// is for. A name the tool does not declare is reported and not fatal: it costs a
    /// sentence, not a call.
    pub fn missing(&self, registry: &crate::tools::ToolRegistry) -> Vec<String> {
        let entries = self.entries();
        let mut out = Vec::new();
        let mut declared: BTreeMap<String, HashSet<String>> = BTreeMap::new();
        for tool in registry.iter() {
            let name = tool.name();
            declared.entry(name.to_string()).or_default();
            if entries
                .get(name)
                .and_then(|entry| entry.description.as_ref())
                .is_none()
            {
                out.push(format!("{name} (what it is)"));
            }
            let schema = crate::tools::general_parameters(tool);
            let Some(properties) = schema.get("properties").and_then(Value::as_object) else {
                continue;
            };
            for argument in properties.keys() {
                declared
                    .entry(name.to_string())
                    .or_default()
                    .insert(argument.clone());
                if sentence(&entries, name, argument).is_none() {
                    out.push(format!("{name}.{argument}"));
                }
            }
        }
        for (tool, entry) in &entries {
            if tool != GENERAL && !declared.contains_key(tool) {
                self.warn_once(format!("tool:{tool}"), || {
                    format!("`{TABLE}.{tool}` words a tool this server does not carry")
                });
            }
            for argument in entry.arguments.keys() {
                let known = declared
                    .get(tool)
                    .or_else(|| declared.get(GENERAL))
                    .is_some_and(|names| names.contains(argument));
                if !known {
                    self.warn_once(format!("argument:{tool}.{argument}"), || {
                        format!(
                            "`{TABLE}.{tool}.arguments.{argument}` names an argument \
                             no tool of that name takes"
                        )
                    });
                }
            }
        }
        out
    }

    fn warn_once(&self, key: String, message: impl FnOnce() -> String) {
        let mut warned = self.warned.lock().unwrap_or_else(|e| e.into_inner());
        if warned.insert(key) {
            tracing::warn!("{}", message());
        }
    }
}

/// One argument's sentence: the tool's own, or the tunnel's.
fn sentence(entries: &BTreeMap<String, Entry>, tool: &str, argument: &str) -> Option<String> {
    entries
        .get(tool)
        .and_then(|entry| entry.arguments.get(argument))
        .or_else(|| {
            entries
                .get(GENERAL)
                .and_then(|entry| entry.arguments.get(argument))
        })
        .cloned()
}

/// The process's words, installed at startup from the home's config.
static SHARED: OnceLock<Texts> = OnceLock::new();

/// Points the process at one home's config, and reads it now.
///
/// # Invariant
///
/// The file is the only source: a missing sentence is a startup failure, never a
/// request that sends a schema with half its words in it.
pub fn install(config: &Path) {
    let _ = SHARED.set(Texts::at(Some(config.to_path_buf())));
    let texts = shared();
    let names: Vec<String> = texts.entries().keys().cloned().collect();
    tracing::info!(
        file = %config.display(),
        tools = ?names,
        "tool words: read from this config, and from nowhere else"
    );
}

/// The words every request is built from; empty until `install` runs.
pub fn shared() -> &'static Texts {
    SHARED.get_or_init(|| Texts::at(None))
}

/// The `[tool_text]` of a TOML text, with whatever was wrong with it.
///
/// # Safety
///
/// A file that does not parse yields nothing, and a server with no words refuses to
/// start rather than guessing: see `Texts::missing`.
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
