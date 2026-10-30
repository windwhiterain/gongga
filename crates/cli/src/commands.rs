//! Sticky arguments and event filters: the client's own state in `cli.toml`.
//!
//! See `docs/notes/cli-commands.md`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use protocol::{Args, BLOCK_SIZES, CommandSpec, Event, SPLIT_VALUES};
use serde::{Deserialize, Serialize};

/// How many wrapped rows one block may fill. Client-side only.
///
/// # Invariant
///
/// `0` caps nothing, and it is the only way to say so: a front-end never has to
/// recognise a sentinel mode or a magic mode name.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct BlockSize(pub usize);

impl BlockSize {
    /// A size, or the way to name one; there is no silent default.
    pub fn parse(raw: &str) -> Result<Self, String> {
        match raw.trim().parse::<usize>() {
            Ok(lines) => Ok(Self(lines)),
            Err(_) => Err(format!(
                "no block size {raw:?}: `0` caps nothing, any other number is the most rows \
                 one block fills; the settings worth typing are {}",
                BLOCK_SIZES.join(", ")
            )),
        }
    }

    /// Loads `home/cli.toml`; a missing or unreadable file caps nothing.
    pub fn load(home: &Path) -> Self {
        read_file(&home.join("cli.toml"))
            .block_size
            .as_deref()
            .and_then(|raw| BlockSize::parse(raw).ok())
            .unwrap_or_default()
    }

    pub fn save(self, home: &Path) -> anyhow::Result<()> {
        let path = home.join("cli.toml");
        let mut file = read_file(&path);
        file.block_size = Some(self.0.to_string());
        write_file(&path, &file)
    }
}

/// Whether each session gets its own events window. Client-side only.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Split(pub bool);

impl Split {
    pub fn as_str(self) -> &'static str {
        if self.0 { "true" } else { "false" }
    }

    /// The value named, or the list of values; there is no silent default.
    pub fn parse(raw: &str) -> Result<Self, String> {
        match raw.trim() {
            "true" => Ok(Split(true)),
            "false" => Ok(Split(false)),
            other => Err(format!(
                "no split value {other:?}; it takes {}",
                SPLIT_VALUES.join(", ")
            )),
        }
    }

    /// Loads `home/cli.toml`; a missing or unreadable file is off.
    pub fn load(home: &Path) -> Self {
        Self(read_file(&home.join("cli.toml")).event_split)
    }

    pub fn save(self, home: &Path) -> anyhow::Result<()> {
        let path = home.join("cli.toml");
        let mut file = read_file(&path);
        file.event_split = self.0;
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
    block_size: Option<String>,
    #[serde(default)]
    event_split: bool,
    /// Where `connect_server` last pointed this client.
    #[serde(default)]
    connection: Option<Stored>,
}

/// The server a client was told to use, as the file spells it.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct Stored {
    #[serde(default)]
    ip: Option<String>,
    port: u16,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Entry {
    name: String,
    value: String,
}

/// The server `connect_server` pointed this client at.
///
/// See `docs/notes/cli-commands.md`.
pub struct Remembered;

impl Remembered {
    /// `None` when nothing was remembered, or the file does not parse.
    pub fn load(home: &Path) -> Option<std::net::SocketAddr> {
        let stored = read_file(&home.join("cli.toml")).connection?;
        // Parsed, not formatted: `"::1:9700"` is not an address, and a file
        // that was right must not fall back to the default.
        let ip: std::net::IpAddr = stored
            .ip
            .as_deref()
            .unwrap_or("127.0.0.1")
            .parse()
            .ok()?;
        Some(std::net::SocketAddr::new(ip, stored.port))
    }

    /// Writes the server the next command should talk to.
    pub fn save(home: &Path, addr: std::net::SocketAddr) -> anyhow::Result<()> {
        let path = home.join("cli.toml");
        let mut file = read_file(&path);
        file.connection = Some(Stored {
            ip: Some(addr.ip().to_string()),
            port: addr.port(),
        });
        write_file(&path, &file)
    }
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

    /// Fills in a required argument the caller omitted, when a sticky value exists.
    ///
    /// # Invariant
    ///
    /// An optional argument is left alone, because absence there is a decision:
    /// `set_event_ui` with no `--session` means "keep the filter as it is", and a
    /// sticky value would make every other setting change it.
    pub fn fill(&self, spec: Option<&CommandSpec>, args: &mut Args) {
        let Some(spec) = spec else { return };
        for a in &spec.args {
            if a.required && !args.contains_key(&a.name) {
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

    /// The size survives a restart; an unknown one on disk falls back rather
    /// than refusing to start.
    #[test]
    fn the_block_size_is_remembered() {
        let d = dir();
        assert_eq!(BlockSize::load(d.path()), BlockSize(0));
        BlockSize(3).save(d.path()).unwrap();
        assert_eq!(BlockSize::load(d.path()), BlockSize(3));

        // Hand-written rubbish is not a reason to fail to start.
        std::fs::write(d.path().join("cli.toml"), "block_size = \"huge\"\n").unwrap();
        assert_eq!(BlockSize::load(d.path()), BlockSize(0));
    }

    #[test]
    fn an_unknown_block_size_says_how_to_name_one() {
        let why = BlockSize::parse("huge").unwrap_err();
        assert!(why.contains("caps nothing"), "{why}");
        // A mode name was the old spelling; a number of lines is the setting now.
        assert!(
            BlockSize::parse(" all ").is_err(),
            "all is not a size any more"
        );
        assert_eq!(BlockSize::parse(" 2 "), Ok(BlockSize(2)));
    }

    /// The setting survives a restart, and only the two words it names are taken.
    #[test]
    fn the_split_setting_is_remembered() {
        let d = dir();
        assert_eq!(Split::load(d.path()), Split(false));
        Split(true).save(d.path()).unwrap();
        assert_eq!(Split::load(d.path()), Split(true));

        assert_eq!(Split::parse(" true "), Ok(Split(true)));
        assert_eq!(
            Split::parse("no"),
            Err("no split value \"no\"; it takes true, false".into())
        );
        // A split is saved without losing the block size beside it.
        BlockSize(3).save(d.path()).unwrap();
        assert_eq!(BlockSize::load(d.path()), BlockSize(3));
        assert_eq!(Split::load(d.path()), Split(true));
    }

    #[test]
    fn fill_only_adds_missing_arguments() {
        let d = dir();
        let mut s = Sticky::load(d.path());
        s.set("session", "main").unwrap();
        s.set("cwd", "/tmp/scratch").unwrap();
        let spec = CommandSpec {
            target: protocol::CommandTarget::Server,
            name: "send_message".into(),
            description: String::new(),
            args: vec![
                ArgSpec {
                    name: "session".into(),
                    description: String::new(),
                    required: true,
                    value_hint: None,
                    completions: protocol::CompletionSource::None,
                },
                // Leaving an optional argument out is the caller's decision, not a
                // gap for a sticky value to fill.
                ArgSpec {
                    name: "cwd".into(),
                    description: String::new(),
                    required: false,
                    value_hint: None,
                    completions: protocol::CompletionSource::None,
                },
            ],
        };
        let mut args = Args::new();
        s.fill(Some(&spec), &mut args);
        assert_eq!(args["session"], "main");
        assert!(
            !args.contains_key("cwd"),
            "a sticky value does not fill what the command made optional: {args:?}"
        );
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
