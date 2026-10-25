//! Sticky arguments and event filters: the client's own state in `cli.toml`.
//!
//! See `docs/notes/cli-commands.md`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use protocol::{Args, CommandSpec, DETAIL_MODES, Event};
use serde::{Deserialize, Serialize};

/// How much of each event the log shows. Client-side only.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Detail {
    /// Every block in full; the default.
    #[default]
    All,
    /// Each block that is not content folded onto one row, rewritten as it streams.
    Line,
    /// One row per session, and one for the command echo; content keeps its text.
    One,
}

impl Detail {
    pub fn as_str(self) -> &'static str {
        match self {
            Detail::All => "all",
            Detail::Line => "line",
            Detail::One => "one",
        }
    }

    /// The mode named, or the list of modes; there is no silent default.
    pub fn parse(raw: &str) -> Result<Self, String> {
        match raw.trim() {
            "all" => Ok(Detail::All),
            "line" => Ok(Detail::Line),
            "one" => Ok(Detail::One),
            other => Err(format!(
                "no event detail mode {other:?}; it takes {}",
                DETAIL_MODES.join(", ")
            )),
        }
    }

    /// Loads `home/cli.toml`; a missing or unreadable file is the default mode.
    pub fn load(home: &Path) -> Self {
        read_file(&home.join("cli.toml"))
            .event_detail
            .as_deref()
            .and_then(|raw| Detail::parse(raw).ok())
            .unwrap_or_default()
    }

    pub fn save(self, home: &Path) -> anyhow::Result<()> {
        let path = home.join("cli.toml");
        let mut file = read_file(&path);
        file.event_detail = Some(self.as_str().to_string());
        write_file(&path, &file)
    }
}

/// The pre-rename key for sticky arguments: read once, then rewritten
/// under `sticky_cmd_arguments`.
const OLD_STICKY_KEY: &str = "persistant_cmd_arguments";

/// `~/.gongga/cli.toml`.
#[derive(Debug, Default, Serialize, Deserialize)]
struct File {
    #[serde(default)]
    sticky_cmd_arguments: Vec<Entry>,
    #[serde(default)]
    event_filters: Vec<Entry>,
    #[serde(default)]
    event_detail: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Entry {
    name: String,
    value: String,
}

pub struct Sticky {
    path: PathBuf,
    args: BTreeMap<String, String>,
}

impl Sticky {
    /// Loads from `home/cli.toml`; a missing or unreadable file yields none.
    pub fn load(home: &Path) -> Self {
        let path = home.join("cli.toml");
        let args = read_file(&path)
            .sticky_cmd_arguments
            .into_iter()
            .map(|e| (e.name, e.value))
            .collect();
        Self { path, args }
    }

    /// The directory whose `cli.toml` this is; event filters live beside it.
    pub fn home(&self) -> &Path {
        self.path.parent().unwrap_or(Path::new("."))
    }

    pub fn names(&self) -> Vec<String> {
        self.args.keys().cloned().collect()
    }

    /// `name` -> `value` pairs, for the status bar.
    pub fn pairs(&self) -> Vec<(String, String)> {
        self.args
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect()
    }

    pub fn get(&self, name: &str) -> Option<&str> {
        self.args.get(name).map(String::as_str)
    }

    pub fn set(&mut self, name: &str, value: &str) -> anyhow::Result<()> {
        self.args.insert(name.to_string(), value.to_string());
        self.save()
    }

    /// Returns false when there was nothing to remove.
    pub fn del(&mut self, name: &str) -> anyhow::Result<bool> {
        match self.args.remove(name) {
            Some(_) => self.save().map(|()| true),
            None => Ok(false),
        }
    }

    fn save(&self) -> anyhow::Result<()> {
        let mut file = read_file(&self.path);
        file.sticky_cmd_arguments = entries(&self.args);
        write_file(&self.path, &file)
    }

    /// Fills in any argument the caller omitted, when a sticky value exists.
    pub fn fill(&self, spec: Option<&CommandSpec>, args: &mut Args) {
        let Some(spec) = spec else { return };
        for a in &spec.args {
            if !args.contains_key(&a.name) {
                if let Some(v) = self.args.get(&a.name) {
                    args.insert(a.name.clone(), v.clone());
                }
            }
        }
    }
}

/// A display filter: each pair narrows the events that carry that field.
pub struct EventFilters {
    path: PathBuf,
    filters: BTreeMap<String, String>,
}

impl EventFilters {
    /// Loads from `home/cli.toml`; a missing or unreadable file yields none.
    pub fn load(home: &Path) -> Self {
        let path = home.join("cli.toml");
        let filters = read_file(&path)
            .event_filters
            .into_iter()
            .map(|e| (e.name, e.value))
            .collect();
        Self { path, filters }
    }

    pub fn names(&self) -> Vec<String> {
        self.filters.keys().cloned().collect()
    }

    pub fn pairs(&self) -> Vec<(String, String)> {
        self.filters
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect()
    }

    pub fn get(&self, name: &str) -> Option<&str> {
        self.filters.get(name).map(String::as_str)
    }

    pub fn set(&mut self, name: &str, value: &str) -> anyhow::Result<()> {
        self.filters.insert(name.to_string(), value.to_string());
        self.save()
    }

    /// Returns false when there was nothing to remove.
    pub fn del(&mut self, name: &str) -> anyhow::Result<bool> {
        match self.filters.remove(name) {
            Some(_) => self.save().map(|()| true),
            None => Ok(false),
        }
    }

    /// An event without the named field is left alone: `session` filters the
    /// streams, not the command echo.
    pub fn allows(&self, event: &Event) -> bool {
        if self.filters.is_empty() {
            return true;
        }
        let Ok(serde_json::Value::Object(fields)) = serde_json::to_value(event) else {
            return true;
        };
        self.filters
            .iter()
            .all(|(name, want)| match fields.get(name) {
                None => true,
                Some(actual) => scalar(actual).as_deref() == Some(want.as_str()),
            })
    }

    fn save(&self) -> anyhow::Result<()> {
        let mut file = read_file(&self.path);
        file.event_filters = entries(&self.filters);
        write_file(&self.path, &file)
    }
}

/// The JSON scalar as the string a filter value is written with.
fn scalar(value: &serde_json::Value) -> Option<String> {
    match value {
        serde_json::Value::String(s) => Some(s.clone()),
        serde_json::Value::Number(n) => Some(n.to_string()),
        serde_json::Value::Bool(b) => Some(b.to_string()),
        serde_json::Value::Null => Some("null".to_string()),
        _ => None,
    }
}

fn entries(args: &BTreeMap<String, String>) -> Vec<Entry> {
    args.iter()
        .map(|(name, value)| Entry {
            name: name.clone(),
            value: value.clone(),
        })
        .collect()
}

/// Reads what is on disk so one writer keeps the other's section.
fn read_file(path: &Path) -> File {
    let Ok(text) = std::fs::read_to_string(path) else {
        return File::default();
    };
    let Ok(mut value) = toml::from_str::<toml::Value>(&text) else {
        return File::default();
    };
    fold_old_sticky_key(&mut value);
    File::deserialize(value).unwrap_or_default()
}

/// Moves the pre-rename key under the current one.
///
/// # Invariant
///
/// A file carrying both keys keeps every pair; on a name clash the current key
/// wins, because that is the one a later write keeps.
fn fold_old_sticky_key(value: &mut toml::Value) {
    let Some(root) = value.as_table_mut() else {
        return;
    };
    let Some(old) = root.remove(OLD_STICKY_KEY) else {
        return;
    };
    let mut merged = old.as_array().cloned().unwrap_or_default();
    match root.get("sticky_cmd_arguments") {
        Some(toml::Value::Array(new)) => merged.extend(new.iter().cloned()),
        // A malformed current key is left for the parser to reject.
        Some(_) => return,
        None => {}
    }
    root.insert(
        "sticky_cmd_arguments".to_string(),
        toml::Value::Array(merged),
    );
}

fn write_file(path: &Path, file: &File) -> anyhow::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, toml::to_string_pretty(file)?)?;
    Ok(())
}

/// `--flag value` or `--flag=value`; nothing positional, and a value may
/// start with `--`.
pub fn parse_args(tokens: &[String]) -> anyhow::Result<Args> {
    let mut args = Args::new();
    let mut i = 0;
    while i < tokens.len() {
        let token = &tokens[i];
        let Some(flag) = token.strip_prefix("--") else {
            anyhow::bail!("unexpected argument {token:?}; flags start with `--`");
        };
        if let Some((name, value)) = flag.split_once('=') {
            args.insert(name.to_string(), value.to_string());
            i += 1;
        } else {
            let value = tokens
                .get(i + 1)
                .ok_or_else(|| anyhow::anyhow!("--{flag} needs a value"))?;
            args.insert(flag.to_string(), value.clone());
            i += 2;
        }
    }
    Ok(args)
}

/// Renders arguments back to a command line, for the echoed `$ ...` line.
pub fn render_args(args: &Args) -> String {
    args.iter()
        .map(|(k, v)| {
            if v.contains(' ') {
                format!(" --{k} \"{v}\"")
            } else {
                format!(" --{k} {v}")
            }
        })
        .collect::<Vec<_>>()
        .join("")
}

#[cfg(test)]
mod tests {
    use super::*;
    use protocol::ArgSpec;

    fn dir() -> tempfile::TempDir {
        tempfile::tempdir().unwrap()
    }

    #[test]
    fn sticky_arguments_survive_a_reload() {
        let d = dir();
        let mut s = Sticky::load(d.path());
        s.set("session", "main").unwrap();
        let again = Sticky::load(d.path());
        assert_eq!(
            again.pairs(),
            vec![("session".to_string(), "main".to_string())]
        );
        assert_eq!(again.names(), vec!["session".to_string()]);
    }

    #[test]
    fn deleting_reports_whether_it_hit() {
        let d = dir();
        let mut s = Sticky::load(d.path());
        s.set("session", "main").unwrap();
        assert!(s.del("session").unwrap());
        assert!(!s.del("session").unwrap());
    }

    /// A `cli.toml` written before the rename still yields its arguments.
    #[test]
    fn the_old_disk_key_is_still_read() {
        let d = dir();
        std::fs::write(
            d.path().join("cli.toml"),
            format!("[[{OLD_STICKY_KEY}]]\nname = \"session\"\nvalue = \"test1\"\n"),
        )
        .unwrap();
        let s = Sticky::load(d.path());
        assert_eq!(s.get("session"), Some("test1"));
    }

    /// Both spellings may be on disk at once.
    ///
    /// # Invariant
    ///
    /// The file parses, every pair survives, and the new key wins a name that
    /// appears in both.
    #[test]
    fn both_disk_keys_load_without_losing_a_pair() {
        let d = dir();
        std::fs::write(
            d.path().join("cli.toml"),
            format!(
                "[[{OLD_STICKY_KEY}]]\nname = \"session\"\nvalue = \"old\"\n\
                 [[{OLD_STICKY_KEY}]]\nname = \"cwd\"\nvalue = \"/old\"\n\
                 [[sticky_cmd_arguments]]\nname = \"session\"\nvalue = \"new\"\n"
            ),
        )
        .unwrap();
        let s = Sticky::load(d.path());
        assert_eq!(
            s.pairs(),
            vec![
                ("cwd".to_string(), "/old".to_string()),
                ("session".to_string(), "new".to_string())
            ]
        );
    }

    /// Whichever writer saves, the file ends up under the new key.
    ///
    /// # Invariant
    ///
    /// Every argument stays intact -- including through an event-filter write,
    /// which never touched the sticky pairs.
    #[test]
    fn any_save_migrates_the_old_key_without_dropping_arguments() {
        let d = dir();
        std::fs::write(
            d.path().join("cli.toml"),
            format!("[[{OLD_STICKY_KEY}]]\nname = \"session\"\nvalue = \"test1\"\n"),
        )
        .unwrap();
        let mut filters = EventFilters::load(d.path());
        filters.set("part", "output").unwrap();

        let text = std::fs::read_to_string(d.path().join("cli.toml")).unwrap();
        assert!(!text.contains(OLD_STICKY_KEY), "{text}");
        assert_eq!(Sticky::load(d.path()).get("session"), Some("test1"));
        assert_eq!(EventFilters::load(d.path()).get("part"), Some("output"));
    }

    /// The two writers share one file, so each has to keep the other's rows.
    #[test]
    fn event_filters_persist_beside_sticky_arguments() {
        let d = dir();
        let mut sticky = Sticky::load(d.path());
        sticky.set("session", "main").unwrap();
        let mut filters = EventFilters::load(d.path());
        filters.set("session", "main").unwrap();

        let sticky = Sticky::load(d.path());
        let filters = EventFilters::load(d.path());
        assert_eq!(sticky.get("session"), Some("main"));
        assert_eq!(filters.get("session"), Some("main"));
    }

    /// A session filter narrows the streams; a command has no session to match.
    #[test]
    fn a_session_filter_hides_other_sessions_but_not_commands() {
        let d = dir();
        let mut filters = EventFilters::load(d.path());
        filters.set("session", "main").unwrap();

        let response = |session: &str| {
            protocol::Event::Response(protocol::ResponseEvent {
                session: session.into(),
                turn: 1,
                part: protocol::ResponsePart::Output,
                text: "hi".into(),
                done: false,
            })
        };
        assert!(filters.allows(&response("main")));
        assert!(!filters.allows(&response("other")));
        assert!(filters.allows(&protocol::Event::Cmd {
            id: 1,
            name: "send_message".into(),
            args: Args::new(),
        }));
    }

    /// Every filter has to match: one session filter plus one level filter.
    #[test]
    fn every_filter_has_to_match() {
        let d = dir();
        let mut filters = EventFilters::load(d.path());
        filters.set("session", "main").unwrap();
        filters.set("part", "output").unwrap();
        let event = |part| {
            protocol::Event::Response(protocol::ResponseEvent {
                session: "main".into(),
                turn: 1,
                part,
                text: "x".into(),
                done: false,
            })
        };
        assert!(filters.allows(&event(protocol::ResponsePart::Output)));
        assert!(!filters.allows(&event(protocol::ResponsePart::Reasoning)));
    }

    /// The mode survives a restart; an unknown one on disk falls back rather
    /// than refusing to start.
    #[test]
    fn the_detail_mode_is_remembered() {
        let d = dir();
        assert_eq!(Detail::load(d.path()), Detail::All);
        Detail::Line.save(d.path()).unwrap();
        assert_eq!(Detail::load(d.path()), Detail::Line);

        // Hand-written rubbish is not a reason to fail to start.
        std::fs::write(d.path().join("cli.toml"), "event_detail = \"huge\"\n").unwrap();
        assert_eq!(Detail::load(d.path()), Detail::All);
    }

    #[test]
    fn an_unknown_detail_mode_lists_the_modes() {
        let why = Detail::parse("huge").unwrap_err();
        assert!(why.contains("all, line, one"), "{why}");
        assert_eq!(Detail::parse(" one "), Ok(Detail::One));
    }

    #[test]
    fn fill_only_adds_missing_arguments() {
        let d = dir();
        let mut s = Sticky::load(d.path());
        s.set("session", "main").unwrap();
        let spec = CommandSpec {
            target: protocol::CommandTarget::Server,
            name: "send_message".into(),
            summary: String::new(),
            args: vec![ArgSpec {
                name: "session".into(),
                summary: String::new(),
                required: false,
                value_hint: None,
                completions: protocol::CompletionSource::None,
            }],
        };
        let mut args = Args::new();
        s.fill(Some(&spec), &mut args);
        assert_eq!(args["session"], "main");
        let mut args = Args::from([("session".to_string(), "scratch".to_string())]);
        s.fill(Some(&spec), &mut args);
        assert_eq!(args["session"], "scratch");
    }

    #[test]
    fn both_flag_forms_parse() {
        assert_eq!(
            parse_args(&["--session".into(), "main".into()]).unwrap()["session"],
            "main"
        );
        assert_eq!(
            parse_args(&["--session=main".into()]).unwrap()["session"],
            "main"
        );
    }

    /// A value that looks like a flag is still a value; `send_message` carries
    /// arbitrary text.
    #[test]
    fn a_value_may_start_with_dashes() {
        let args = parse_args(&["--message".into(), "--version".into()]).unwrap();
        assert_eq!(args["message"], "--version");
        let args = parse_args(&["--message=--version".into()]).unwrap();
        assert_eq!(args["message"], "--version");
    }

    #[test]
    fn dangling_flags_are_rejected() {
        assert!(parse_args(&["--session".into()]).is_err());
        assert!(parse_args(&["oops".into()]).is_err());
    }
}
