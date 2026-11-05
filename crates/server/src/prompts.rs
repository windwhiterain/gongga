//! The prompt templates: every word this server hands a model as prose.
//!
//! See `docs/notes/server-prompts.md`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::SystemTime;

/// What introduces a patch for an entry of the index that has changed since.
///
/// # Invariant
///
/// Deliberately not in [`REQUIRED`]: a home that predates this template indexes and
/// re-sends as before, rather than refusing to start over prose it may never need.
pub const REVISION: &str = "reference_revision";

/// The start card: what the session is, where it stands, and when. Stamped once.
pub const TAIL: &str = "dynamic_tail";

/// What opens the request, before the index. See `docs/reference/prefix-cache.md`.
pub const INDEX_OPENING: &str = "index_opening";

/// The other-role message between the opening and the index, so they do not merge.
pub const INDEX_SEPARATOR: &str = "index_separator";

/// The same card when the session's workspace is on a peer.
///
/// # Invariant
///
/// A second template, not a conditional inside the first: a session on this machine
/// keeps the bytes it has already sent, and a prompt with two shapes is two templates
/// named by the code. See `docs/notes/server-prompts.md`.
pub const TAIL_REMOTE: &str = "dynamic_tail_remote";

/// The same card when the peer's directory is not known: stating it is optional.
/// See `docs/notes/server-prompts.md`.
pub const TAIL_REMOTE_UNSTATED: &str = "dynamic_tail_remote_unstated";

/// The system instructions: prose as written, no value filled in; per-session facts
/// go in the card.
///
/// # Invariant
///
/// No per-session or per-turn fact belongs here: these bytes open every request, so a
/// value ends the prefix a provider holds at the byte it changes. A provider caches a
/// byte prefix at an endpoint, shared by every request that sends it; the per-session
/// `prefix_layout` row is our order for the index, not that cache. See
/// `docs/notes/server-prefix-cache.md`.
pub const SYSTEM: &str = "instructions";

/// Every template the code asks for by name.
///
/// # Invariant
///
/// Neither this binary nor any Rust file holds the prose: it lives in the home's
/// config and in the pinned ship content behind it. So a home whose config says
/// nothing about prompts, and whose pinned content cannot be read, would send an
/// empty system prompt and look healthy. Startup asks for this list instead.
pub const REQUIRED: &[&str] = &[
    SYSTEM,
    "context_notice",
    "interrupted_call",
    "size_hint",
    "image_refusal",
    "reference_index",
    "dynamic_tail",
    "dynamic_tail_remote",
    "dynamic_tail_remote_unstated",
    "output_cap",
    "output_omitted",
    INDEX_OPENING,
    INDEX_SEPARATOR,
];

/// The one file section this module reads.
const TABLE: &str = "prompts";

/// What was on disk when a snapshot was read: enough to notice an edit without
/// hashing the file.
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

/// The templates read from the home, plus what has already been complained about.
#[derive(Default)]
struct Snapshot {
    stamp: Option<Stamp>,
    templates: BTreeMap<String, String>,
    complained: bool,
}

/// The prompt prose in force: the home's `[prompts]` over the pinned defaults.
pub struct Prompts {
    path: Option<PathBuf>,
    pinned: BTreeMap<String, String>,
    snapshot: Mutex<Snapshot>,
}

impl Prompts {
    /// A fixed set of text: what a test, or any caller holding the prose, uses.
    pub fn of(templates: &[(&str, &str)]) -> Self {
        Self::layered(
            None,
            templates
                .iter()
                .map(|(name, text)| (name.to_string(), text.to_string()))
                .collect(),
        )
    }

    fn layered(path: Option<PathBuf>, pinned: BTreeMap<String, String>) -> Self {
        Self {
            path,
            pinned,
            snapshot: Mutex::new(Snapshot::default()),
        }
    }

    /// Templates whose text differs from the pinned content: a home that never
    /// mentions prompts overrides none.
    pub fn overridden(&self) -> Vec<String> {
        let mut names: Vec<String> = self
            .templates()
            .into_iter()
            .filter(|(name, text)| self.pinned.get(name) != Some(text))
            .map(|(name, _)| name)
            .collect();
        names.sort();
        names
    }

    /// The required templates nobody supplied, in order: an empty list is the
    /// proof that this server can say anything at all.
    pub fn missing(&self) -> Vec<&'static str> {
        REQUIRED
            .iter()
            .copied()
            .filter(|name| self.text(name).map(|t| t.trim().is_empty()).unwrap_or(true))
            .collect()
    }

    /// One template's text as it stands now, or nothing when no such template exists.
    pub fn text(&self, name: &str) -> Option<String> {
        self.templates()
            .get(name)
            .cloned()
            .or_else(|| self.pinned.get(name).cloned())
    }

    /// The system instructions as written: one text, no value is filled in.
    ///
    /// # Invariant
    ///
    /// Nothing is substituted here, checked rather than trusted: a placeholder in the
    /// head would cut the prefix every session of this directory shares, so it is
    /// reported and left written. See [`SYSTEM`].
    pub fn system(&self) -> String {
        let text = self.text(SYSTEM).unwrap_or_default();
        if text.contains("{{") {
            tracing::error!(
                "the system instructions carry a placeholder: these bytes open every \
                 request, so a value here ends the prefix every session of a directory \
                 shares. Drop the sentence; the card (`dynamic_tail`) states the name"
            );
        }
        text
    }

    /// One template, with its placeholders filled.
    pub fn render(&self, name: &str, vars: &[(&str, &str)]) -> String {
        match self.text(name) {
            Some(text) => substitute(&text, vars),
            None => {
                tracing::warn!(
                    prompt = %name,
                    "no template of that name in the home config or in the pinned ship content"
                );
                String::new()
            }
        }
    }

    /// The home's own `[prompts]`, re-read only when the file says it changed.
    fn templates(&self) -> BTreeMap<String, String> {
        let Some(path) = &self.path else {
            return BTreeMap::new();
        };
        let stamp = Stamp::of(path);
        let mut snapshot = self.snapshot.lock().unwrap_or_else(|e| e.into_inner());
        if snapshot.stamp == stamp && stamp.is_some() {
            return snapshot.templates.clone();
        }
        let (templates, problems) = match std::fs::read_to_string(path) {
            Ok(text) => read_table(&text),
            Err(e) => {
                tracing::warn!(
                    file = %path.display(),
                    error = %e,
                    "cannot read the prompt templates; the pinned defaults stand"
                );
                (BTreeMap::new(), Vec::new())
            }
        };
        // An unreadable file yields nothing, so the pinned content stands --
        // complained about once, not on every request.
        if problems.is_empty() || !snapshot.complained {
            for problem in &problems {
                tracing::warn!(file = %path.display(), problem = %problem, "prompt template ignored");
            }
            snapshot.complained = !problems.is_empty();
        }
        for name in templates.keys() {
            if !self.pinned.contains_key(name) && !REQUIRED.contains(&name.as_str()) {
                tracing::warn!(prompt = %name, "no prompt uses this template; is the name misspelled?");
            }
        }
        snapshot.stamp = stamp;
        snapshot.templates = templates;
        snapshot.templates.clone()
    }
}

/// The process's templates, installed at startup from the home's config.
static SHARED: OnceLock<Prompts> = OnceLock::new();

/// Points the process at one home's config; startup checks what came back.
pub fn install(config: &Path) -> Vec<&'static str> {
    let pinned = match crate::ship::config_text() {
        Some(text) => read_table(&text).0,
        None => {
            tracing::warn!(
                content = crate::ship::resolved_commit().unwrap_or("nothing resolved"),
                protocol = crate::ship::link().protocol,
                "the shipped config is not readable here; only this home's [prompts] stand"
            );
            BTreeMap::new()
        }
    };
    let _ = SHARED.set(Prompts::layered(Some(config.to_path_buf()), pinned));
    let prompts = shared();
    // Reads the home now, so the first request is not the first read.
    let _ = prompts.templates();
    prompts.missing()
}

/// The templates every request is built from; empty until `install` runs.
pub fn shared() -> &'static Prompts {
    SHARED.get_or_init(|| Prompts::of(&[]))
}

/// Fills `{{name}}` in one template.
///
/// # Invariant
///
/// A placeholder the caller does not supply is left in the text exactly as
/// written: dropping it would delete a sentence the model still has to read,
/// and the reader of the request is the one who can tell a typo from a value.
pub fn substitute(text: &str, vars: &[(&str, &str)]) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find("{{") {
        out.push_str(&rest[..at]);
        let tail = &rest[at + 2..];
        let Some(end) = tail.find("}}") else {
            out.push_str(&rest[at..]);
            return out;
        };
        let name = &tail[..end];
        match vars.iter().find(|(key, _)| *key == name) {
            Some((_, value)) => out.push_str(value),
            None => out.push_str(&rest[at..at + end + 4]),
        }
        rest = &tail[end + 2..];
    }
    out.push_str(rest);
    out
}

/// The `[prompts]` of a TOML text, with whatever was wrong with the rest of it.
///
/// # Safety
///
/// A file that does not parse yields nothing: the pinned defaults are the answer
/// to a broken config, not a server that cannot start.
fn read_table(text: &str) -> (BTreeMap<String, String>, Vec<String>) {
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
    let Some(entries) = value.as_table() else {
        problems.push(format!("[{TABLE}] is not a table"));
        return (out, problems);
    };
    for (name, value) in entries {
        match value.as_str() {
            Some(body) => {
                out.insert(name.clone(), body.to_string());
            }
            None => problems.push(format!("`{name}` is not a string, so it is ignored")),
        }
    }
    (out, problems)
}
