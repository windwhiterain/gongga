//! The prompt templates: every word this server hands a model as prose.
//!
//! See `docs/notes/server-prompts.md`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::SystemTime;

/// The config that ships with the binary: its `[prompts]` are every default.
const SHIPPED: &str = include_str!("../default.config.toml");

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
}

/// The prompt prose in force: the home's `[prompts]` over the shipped defaults.
pub struct Prompts {
    path: Option<PathBuf>,
    shipped: BTreeMap<String, String>,
    snapshot: Mutex<Snapshot>,
}

impl Prompts {
    /// Reads `[prompts]` from `config` beside the shipped defaults, lazily.
    pub fn new(config: &Path) -> Self {
        Self::build(Some(config.to_path_buf()))
    }

    /// The defaults alone: what a test or a tool uses when no home is in play.
    pub fn defaults() -> Self {
        Self::build(None)
    }

    fn build(path: Option<PathBuf>) -> Self {
        Self {
            path,
            shipped: read_table(SHIPPED).0,
            snapshot: Mutex::new(Snapshot::default()),
        }
    }

    /// The templates whose text differs from the shipped default: a new home is
    /// a copy of that file, so only a wording someone changed counts.
    pub fn overridden(&self) -> Vec<String> {
        let mut names: Vec<String> = self
            .templates()
            .into_iter()
            .filter(|(name, text)| self.shipped.get(name) != Some(text))
            .map(|(name, _)| name)
            .collect();
        names.sort();
        names
    }

    /// One template's text as it stands now, or nothing when no such template exists.
    pub fn text(&self, name: &str) -> Option<String> {
        self.templates()
            .get(name)
            .cloned()
            .or_else(|| self.shipped.get(name).cloned())
    }

    /// One template, with its placeholders filled.
    pub fn render(&self, name: &str, vars: &[(&str, &str)]) -> String {
        match self.text(name) {
            Some(text) => substitute(&text, vars),
            None => {
                tracing::warn!(
                    prompt = %name,
                    "no template of that name in the home config or in the shipped defaults"
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
        let mut snapshot = self
            .snapshot
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        if snapshot.stamp == stamp && stamp.is_some() {
            return snapshot.templates.clone();
        }
        let (templates, problems) = match std::fs::read_to_string(path) {
            Ok(text) => read_table(&text),
            Err(e) => {
                tracing::warn!(
                    file = %path.display(),
                    error = %e,
                    "cannot read the prompt templates; the shipped defaults stand"
                );
                (BTreeMap::new(), Vec::new())
            }
        };
        for problem in problems {
            tracing::warn!(file = %path.display(), problem = %problem, "prompt template ignored");
        }
        for name in templates.keys() {
            if !self.shipped.contains_key(name) {
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

/// Points the whole process at one home's config. Called once, at startup.
pub fn install(config: &Path) {
    let _ = SHARED.set(Prompts::new(config));
}

/// The templates every request is built from; the defaults until `install` runs.
pub fn shared() -> &'static Prompts {
    SHARED.get_or_init(Prompts::defaults)
}

/// The rules a project wrote for its agents, read now and wrapped for one
/// request.
///
/// # Invariant
///
/// Read again for every request and never stored: these rules join the
/// `instructions` of the next request, and no row is written to a session's
/// transcript, so the history of a conversation under way stays exactly what
/// the user and the model said.
pub fn constitution(cwd: &Path) -> String {
    let prompts = shared();
    let mut parts = Vec::new();
    for path in machine_file().into_iter().chain(std::iter::once(cwd.join("AGENTS.md"))) {
        match std::fs::read_to_string(&path) {
            Ok(text) if !text.trim().is_empty() => parts.push(prompts.render(
                "constitution",
                &[("path", path.display().to_string().as_str()), ("content", text.trim())],
            )),
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => {
                tracing::warn!(file = %path.display(), error = %e, "cannot read the project's agent rules")
            }
        }
    }
    parts.join("\n\n")
}

/// Where the machine's own rules live, beside the roster it also keeps.
fn machine_file() -> Option<PathBuf> {
    dirs::home_dir().map(|h| h.join(".agents/agents.md"))
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

/// One machine value written into the starter config.
#[derive(Debug, Clone)]
pub enum Value {
    /// Text, written between the quotes the placeholder sits in.
    Text(String),
    /// A number or bool, replacing the quoted placeholder whole: TOML reads
    /// `"false"` as text and a config wants the literal.
    Bare(String),
}

impl Value {
    pub fn text(value: impl Into<String>) -> Self {
        Value::Text(value.into())
    }

    pub fn bare(value: impl std::fmt::Display) -> Self {
        Value::Bare(value.to_string())
    }
}

/// The starter config for one home: the shipped file with this machine's values
/// filled in.
///
/// # Invariant
///
/// Everything from the `[prompts]` header down is appended exactly as shipped:
/// those placeholders belong to a later request, not to startup, and filling
/// them here would freeze the templates to this one home and this one model.
pub fn starter(vars: &[(&str, Value)]) -> String {
    let head_end = SHIPPED
        .find("\n[prompts]")
        .map(|at| at + 1)
        .unwrap_or(SHIPPED.len());
    let mut head = SHIPPED[..head_end].to_string();
    for (name, value) in vars {
        let placeholder = format!("{{{{{name}}}}}");
        let (literal, quoted) = match value {
            Value::Text(text) => (text.as_str(), false),
            Value::Bare(literal) => (literal.as_str(), true),
        };
        if quoted {
            // The quoted form first: it is a whole value losing its quotes.
            head = head.replace(&format!("\"{placeholder}\""), literal);
        }
        head = head.replace(&placeholder, literal);
    }
    head.push_str(&SHIPPED[head_end..]);
    head
}

/// The `[prompts]` of a TOML text, with whatever was wrong with the rest of it.
///
/// # Safety
///
/// A file that does not parse yields nothing: the shipped defaults are the
/// answer to a broken config, not a server that cannot start.
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

#[cfg(test)]
mod tests {
    /// The project's rules reach the prompt through its template, and an
    /// edit is seen without a restart.
    #[test]
    fn the_session_rules_join_the_prompt() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("AGENTS.md"), "- never force push\n").unwrap();
        let said = super::constitution(dir.path());
        assert!(said.contains("never force push"), "{said}");
        assert!(
            said.contains("The rules this project wrote"),
            "the wrapper is part of what the model reads: {said}"
        );
        std::fs::write(dir.path().join("AGENTS.md"), "- never rebase alone\n").unwrap();
        let again = super::constitution(dir.path());
        assert!(again.contains("never rebase alone"), "{again}");
        assert!(!again.contains("never force push"), "{again}");
    }

    use super::*;

    #[test]
    fn a_placeholder_becomes_the_value_named_for_it() {
        assert_eq!(
            substitute("for {{session}} at {{tokens}}/{{budget}}", &[
                ("session", "main"),
                ("tokens", "41"),
                ("budget", "40"),
            ]),
            "for main at 41/40"
        );
    }

    /// The whole point of leaving it: a missing value shows up rather than
    /// deleting the sentence around it.
    #[test]
    fn an_unsupplied_placeholder_stays_in_the_text() {
        assert_eq!(substitute("fold {{session}} now", &[]), "fold {{session}} now");
        assert_eq!(
            substitute("a {{session}} b", &[("other", "x")]),
            "a {{session}} b"
        );
    }

    /// A placeholder with no closing pair is text, not a hole to fill.
    #[test]
    fn a_placeholder_with_no_end_is_kept_as_written() {
        assert_eq!(substitute("dangling {{session", &[("session", "main")]), "dangling {{session");
    }

    /// The starter a new home gets carries this machine's values in the config
    /// and nobody's values in the templates.
    #[test]
    fn the_starter_fills_values_and_leaves_the_templates_alone() {
        let filled = starter(&[
            ("bind", Value::text("127.0.0.1:1234")),
            ("mtu", Value::bare(8000)),
        ]);
        assert!(filled.contains("bind = \"127.0.0.1:1234\""), "{filled}");
        assert!(filled.contains("mtu = 8000"), "{filled}");
        assert!(
            filled.contains("Context notice for `{{session}}`"),
            "a template is not a startup value"
        );
        assert!(starter(&[]).contains("mtu = \"{{mtu}}\""));
    }

    /// Every shipped default has text, and the set is the one the code asks for.
    #[test]
    fn the_shipped_defaults_cover_every_template_the_server_asks_for() {
        let prompts = Prompts::defaults();
        for name in [
            "instructions",
            "constitution",
            "summary_preamble",
            "context_notice",
            "folded_none",
            "folded_through",
            "interrupted_call",
            "size_hint",
            "image_refusal",
            "output_cap",
            "output_omitted",
        ] {
            assert!(!prompts.text(name).unwrap_or_default().is_empty(), "{name}");
        }
        // The shipped set is exactly what the code asks for: a template nobody
        // reads is a template nobody maintains.
        assert!(Prompts::defaults().overridden().is_empty());
    }

    /// The home says which templates it wrote, and nothing else counts.
    #[test]
    fn the_home_reports_what_it_overrode() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "[prompts]\nsize_hint = \"a\"\noutput_cap = \"b\"\n").unwrap();
        let prompts = Prompts::new(&path);
        assert_eq!(prompts.overridden(), ["output_cap".to_string(), "size_hint".to_string()]);
    }

    /// A home that says nothing about prompts still gets the defaults, and a
    /// home that overrides one keeps the rest.
    #[test]
    fn the_home_overrides_one_template_and_inherits_the_rest() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(
            &path,
            "[prompts]\ninterrupted_call = \"asked {{session}}, got nothing\"\n",
        )
        .unwrap();
        let prompts = Prompts::new(&path);
        assert_eq!(
            prompts.render("interrupted_call", &[("session", "main")]),
            "asked main, got nothing"
        );
        assert!(
            prompts
                .text("instructions")
                .unwrap()
                .starts_with("You are gongga"),
            "the shipped default stands where the home is silent"
        );
    }

    /// Real-time means the next request sees the edit, without a restart.
    #[test]
    fn an_edit_lands_without_reopening_anything() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "[prompts]\nsize_hint = \"first\"\n").unwrap();
        let prompts = Prompts::new(&path);
        assert_eq!(prompts.render("size_hint", &[("message", "m")]), "first");
        std::fs::write(&path, "[prompts]\nsize_hint = \"second {{message}}\"\n").unwrap();
        assert_eq!(prompts.render("size_hint", &[("message", "m")]), "second m");
    }

    /// A file that no longer parses is damage, not a reason to stop: the
    /// defaults answer, and the request still goes out.
    #[test]
    fn a_broken_config_falls_back_to_the_defaults_it_cannot_read() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "[prompts]\nsize_hint = \"first\"\n").unwrap();
        let prompts = Prompts::new(&path);
        assert_eq!(prompts.render("size_hint", &[("message", "m")]), "first");
        std::fs::write(&path, "[prompts\nnot toml at all").unwrap();
        assert!(
            prompts
                .render("size_hint", &[("message", "m")])
                .contains("no longer fits")
        );
    }

    /// A non-string is a mistake in the file; the default for that name stands
    /// and the rest of the table is still read.
    #[test]
    fn a_template_that_is_not_text_is_ignored_not_fatal() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "[prompts]\nsize_hint = 7\noutput_cap = \"kept\"\n").unwrap();
        let prompts = Prompts::new(&path);
        assert_eq!(prompts.render("output_cap", &[("omitted", "9")]), "kept");
        assert!(
            prompts
                .render("size_hint", &[("message", "m")])
                .contains("no longer fits")
        );
    }
}
