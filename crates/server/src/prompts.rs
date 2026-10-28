//! The prompt templates: every word this server hands a model as prose.
//!
//! See `docs/notes/server-prompts.md`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::SystemTime;

/// Every template the code asks for by name.
///
/// # Invariant
///
/// Neither this binary nor any Rust file holds the prose: it lives in the home's
/// config and in the pinned ship content behind it. So a home whose config says
/// nothing about prompts, and whose pinned content cannot be read, would send an
/// empty system prompt and look healthy. Startup asks for this list instead.
pub const REQUIRED: &[&str] = &[
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
            .filter(|name| {
                self.text(name)
                    .map(|t| t.trim().is_empty())
                    .unwrap_or(true)
            })
            .collect()
    }

    /// One template's text as it stands now, or nothing when no such template exists.
    pub fn text(&self, name: &str) -> Option<String> {
        self.templates()
            .get(name)
            .cloned()
            .or_else(|| self.pinned.get(name).cloned())
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
                commit = %crate::ship::pinned().commit,
                "the pinned ship config is not readable here; only this home's [prompts] stand"
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

/// The rules a project wrote for its agents, wrapped for one prompt row.
///
/// # Invariant
///
/// Read when a prompt row is written -- a turn's first step, a handover that
/// moved, a directory that changed -- and never re-rendered for a step that
/// stores nothing. A row in the database then says what was sent, which is the
/// only way a transcript can be replayed.
pub fn constitution(prompts: &Prompts, cwd: &Path) -> String {
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

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::*;

    /// A template's placeholders are what the caller fills; the text around them
    /// belongs to whoever wrote the file.
    const NOTICE: &str = "Context notice for `{{session}}`: {{tokens}} of {{budget}}. \
         Fold it with `transcript.py collapse --session {{session}}`.";

    /// A home's file alone: what a home gets before any pinned content.
    fn at(path: &Path) -> Prompts {
        Prompts::layered(Some(path.to_path_buf()), BTreeMap::new())
    }

    fn layered(dir: &Path, pinned: &[(&str, &str)]) -> Prompts {
        Prompts::layered(
            Some(dir.join("config.toml")),
            pinned
                .iter()
                .map(|(name, text)| (name.to_string(), text.to_string()))
                .collect(),
        )
    }

    /// The project's rules reach the prompt through its template, and an
    /// edit is seen without a restart.
    #[test]
    fn the_session_rules_join_the_prompt() {
        let dir = tempfile::tempdir().unwrap();
        let prompts = Prompts::of(&[(
            "constitution",
            "The rules this project wrote at {{path}}:\n\n{{content}}",
        )]);
        std::fs::write(dir.path().join("AGENTS.md"), "- never force push\n").unwrap();
        let said = super::constitution(&prompts, dir.path());
        assert!(said.contains("never force push"), "{said}");
        assert!(
            said.contains("The rules this project wrote"),
            "the wrapper is part of what the model reads: {said}"
        );
        std::fs::write(dir.path().join("AGENTS.md"), "- never rebase alone\n").unwrap();
        let again = super::constitution(&prompts, dir.path());
        assert!(again.contains("never rebase alone"), "{again}");
        assert!(!again.contains("never force push"), "{again}");
    }

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

    /// The check is only as good as the list: one name per template, no
    /// duplicates, every name asked for.
    #[test]
    fn the_required_list_names_each_template_once() {
        let unique: BTreeSet<&str> = REQUIRED.iter().copied().collect();
        assert_eq!(unique.len(), REQUIRED.len(), "{REQUIRED:?}");
        for name in REQUIRED {
            assert!(
                Prompts::of(&[]).missing().contains(name),
                "{name} is not checked when nothing supplies it"
            );
        }
    }

    /// With nothing anywhere every name is missing -- why startup refuses.
    #[test]
    fn a_server_with_no_prose_at_all_says_so() {
        let dir = tempfile::tempdir().unwrap();
        let prompts = at(&dir.path().join("config.toml"));
        assert_eq!(prompts.missing(), REQUIRED.to_vec());
    }

    /// The pinned content alone is enough: a home that never mentions prompts
    /// overrides none of it.
    #[test]
    fn the_pinned_content_alone_answers_the_check() {
        let dir = tempfile::tempdir().unwrap();
        let pinned: Vec<(&str, &str)> = REQUIRED.iter().map(|n| (*n, "x")).collect();
        let prompts = layered(dir.path(), &pinned);
        assert!(prompts.missing().is_empty());
        assert!(prompts.overridden().is_empty());
    }

    /// The home says which templates it wrote, and nothing else counts.
    #[test]
    fn the_home_reports_what_it_overrode() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("config.toml"), "[prompts]\nsize_hint = \"a\"\noutput_cap = \"b\"\n").unwrap();
        let prompts = layered(dir.path(), &[("size_hint", "z"), ("instructions", "keep")]);
        assert_eq!(prompts.overridden(), ["output_cap".to_string(), "size_hint".to_string()]);
    }

    /// A home that overrides one keeps the rest of the pinned prose.
    #[test]
    fn the_home_overrides_one_template_and_inherits_the_rest() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("config.toml"),
            "[prompts]\ninterrupted_call = \"asked {{session}}, got nothing\"\n",
        )
        .unwrap();
        let prompts = layered(dir.path(), &[("instructions", "You are gongga")]);
        assert_eq!(
            prompts.render("interrupted_call", &[("session", "main")]),
            "asked main, got nothing"
        );
        assert_eq!(
            prompts.text("instructions").unwrap(),
            "You are gongga",
            "the pinned default stands where the home is silent"
        );
    }

    /// Real-time means the next request sees the edit, without a restart.
    #[test]
    fn an_edit_lands_without_reopening_anything() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "[prompts]\nsize_hint = \"first\"\n").unwrap();
        let prompts = at(&path);
        assert_eq!(prompts.render("size_hint", &[("message", "m")]), "first");
        std::fs::write(&path, "[prompts]\nsize_hint = \"second {{message}}\"\n").unwrap();
        assert_eq!(prompts.render("size_hint", &[("message", "m")]), "second m");
    }

    /// A file that stopped parsing is damage, not a reason to stop.
    #[test]
    fn a_broken_config_falls_back_to_the_prose_it_cannot_read() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "[prompts]\nsize_hint = \"the home's own {{message}}\"\n").unwrap();
        let prompts = layered(dir.path(), &[("size_hint", "the pinned {{message}}")]);
        assert_eq!(prompts.render("size_hint", &[("message", "m")]), "the home's own m");
        std::fs::write(&path, "[prompts\nnot toml at all").unwrap();
        assert_eq!(
            prompts.render("size_hint", &[("message", "m")]),
            "the pinned m",
            "an unreadable file must not silence the server"
        );
        // And a home that only ever had one template is still not missing it.
        assert_eq!(prompts.render("size_hint", &[("message", "m")]), "the pinned m");
    }

    /// A non-string is a mistake in the file; the pinned text for that name
    /// stands and the rest of the table is still read.
    #[test]
    fn a_template_that_is_not_text_is_ignored_not_fatal() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("config.toml"),
            "[prompts]\nsize_hint = 7\noutput_cap = \"kept\"\n",
        )
        .unwrap();
        let prompts = layered(dir.path(), &[("size_hint", NOTICE)]);
        assert_eq!(prompts.render("output_cap", &[("omitted", "9")]), "kept");
        assert!(prompts
            .render("size_hint", &[("message", "m")])
            .contains("transcript.py collapse"));
    }

    /// A template the home wrote that no code asks for is a typo worth naming --
    /// but one on the required list never is.
    #[test]
    fn a_template_the_server_never_asks_for_is_reported_once() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(&dir.path().join("config.toml"), "[prompts]\nsizehint = \"x\"\n").unwrap();
        let prompts = at(&dir.path().join("config.toml"));
        assert_eq!(prompts.text("sizehint").unwrap(), "x");
        assert!(prompts.missing().contains(&"size_hint"));
    }
}
