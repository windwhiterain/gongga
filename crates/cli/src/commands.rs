//! Sticky arguments and render modes: the client's own state in `cli.toml`.
//!
//! See `docs/notes/cli-commands.md`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use protocol::{Args, BLOCK_SIZES, CommandSpec};
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

/// How each session is drawn: `set_render_session --mode`.
///
/// # Invariant
///
/// It is a view, never a deletion: a session in mode `none` keeps every block it
/// ever carried, so giving it `full` or `line` brings the whole history back.
/// A session nobody set is `full`: a window has to show something.
pub struct RenderModes {
    path: PathBuf,
    modes: BTreeMap<String, crate::RenderMode>,
}

impl RenderModes {
    /// Loads `home/cli.toml`; a missing or unreadable file draws nothing.
    pub fn load(home: &Path) -> Self {
        let path = home.join("cli.toml");
        let modes = read_file(&path)
            .render_session
            .into_iter()
            .filter_map(|e| crate::RenderMode::parse(&e.value).ok().map(|m| (e.name, m)))
            .collect();
        Self { path, modes }
    }

    /// The sessions in this mode, sorted, which is the order a list is read in.
    pub fn named(&self, mode: crate::RenderMode) -> Vec<String> {
        self.modes
            .iter()
            .filter(|(_, m)| **m == mode)
            .map(|(name, _)| name.clone())
            .collect()
    }

    /// What this session is drawn as; a session nobody set is `none`.
    pub fn mode_of(&self, name: &str) -> crate::RenderMode {
        self.modes.get(name).copied().unwrap_or_default()
    }

    pub fn set(&mut self, name: &str, mode: crate::RenderMode) -> anyhow::Result<()> {
        self.modes.insert(name.to_string(), mode);
        self.save()
    }

    fn save(&self) -> anyhow::Result<()> {
        let mut file = read_file(&self.path);
        file.render_session = entries_mode(&self.modes);
        write_file(&self.path, &file)
    }
}

/// The modes as the file spells them, sorted by session so a diff reads.
fn entries_mode(modes: &BTreeMap<String, crate::RenderMode>) -> Vec<Entry> {
    modes
        .iter()
        .map(|(name, mode)| Entry {
            name: name.clone(),
            value: mode.as_str().to_string(),
        })
        .collect()
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
    block_size: Option<String>,
    /// `set_render_session`: the mode each named session is drawn in.
    #[serde(default)]
    render_session: Vec<Entry>,
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
        let ip: std::net::IpAddr = stored.ip.as_deref().unwrap_or("127.0.0.1").parse().ok()?;
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
        file.sticky_cmd_arguments = self
            .args
            .iter()
            .map(|(name, value)| Entry {
                name: name.clone(),
                value: value.clone(),
            })
            .collect();
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

/// The skills `cwd` can open: the project's, the home's, then the machine's.
///
/// # Invariant
///
/// The same three roots, in the same order, that the server walks for
/// `index_skill` and `@skill:` -- so the list a reader completes is the list that
/// can be opened. See `docs/notes/cli-commands.md`.
pub fn installed_skills(home: &Path, cwd: &Path) -> Vec<String> {
    let mut names: Vec<String> = Vec::new();
    for root in [
        cwd.join(".agents").join("skills"),
        home.join("skills"),
        dirs::home_dir()
            .map(|h| h.join(".agents").join("skills"))
            .unwrap_or_default(),
    ] {
        let Ok(entries) = std::fs::read_dir(root) else {
            continue;
        };
        let mut here: Vec<String> = entries
            .flatten()
            .map(|entry| entry.path())
            .filter(|dir| dir.join("SKILL.md").is_file())
            .filter_map(|dir| {
                // The skill's own name wins, as it does in the server's table.
                let text = std::fs::read_to_string(dir.join("SKILL.md")).ok();
                text.as_deref()
                    .and_then(name_in_frontmatter)
                    .or_else(|| dir.file_name().map(|n| n.to_string_lossy().into_owned()))
            })
            .collect();
        here.sort();
        names.extend(here);
    }
    names
}

/// The `name:` a skill's frontmatter declares, when it declares one.
fn name_in_frontmatter(text: &str) -> Option<String> {
    let mut lines = text.lines();
    if lines.next()?.trim() != "---" {
        return None;
    }
    lines
        .take_while(|line| line.trim() != "---")
        .find_map(|line| line.strip_prefix("name:"))
        .map(|rest| rest.trim().trim_matches('"').to_string())
        .filter(|name| !name.is_empty())
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
    /// Every argument stays intact -- including through a render-mode write, which
    /// never touched the sticky pairs.
    #[test]
    fn any_save_migrates_the_old_key_without_dropping_arguments() {
        let d = dir();
        std::fs::write(
            d.path().join("cli.toml"),
            format!("[[{OLD_STICKY_KEY}]]\nname = \"session\"\nvalue = \"test1\"\n"),
        )
        .unwrap();
        let mut modes = RenderModes::load(d.path());
        modes.set("main", crate::RenderMode::Full).unwrap();

        let text = std::fs::read_to_string(d.path().join("cli.toml")).unwrap();
        assert!(!text.contains(OLD_STICKY_KEY), "{text}");
        assert_eq!(Sticky::load(d.path()).get("session"), Some("test1"));
        assert_eq!(
            RenderModes::load(d.path()).mode_of("main"),
            crate::RenderMode::Full
        );
    }

    /// The two writers share one file, so each has to keep the other's rows.
    #[test]
    fn render_modes_persist_beside_sticky_arguments() {
        let d = dir();
        let mut sticky = Sticky::load(d.path());
        sticky.set("session", "main").unwrap();
        let mut modes = RenderModes::load(d.path());
        modes.set("peer", crate::RenderMode::Line).unwrap();

        let sticky = Sticky::load(d.path());
        let modes = RenderModes::load(d.path());
        assert_eq!(sticky.get("session"), Some("main"));
        assert_eq!(modes.mode_of("peer"), crate::RenderMode::Line);
        assert_eq!(
            modes.mode_of("nobody"),
            crate::RenderMode::Full,
            "a session nobody set is drawn in full"
        );
    }

    /// The modes are a view: setting one, then another, keeps them apart.
    #[test]
    fn each_session_keeps_its_own_mode() {
        let d = dir();
        let mut modes = RenderModes::load(d.path());
        modes.set("main", crate::RenderMode::Full).unwrap();
        modes.set("peer", crate::RenderMode::Line).unwrap();
        let modes = RenderModes::load(d.path());
        assert_eq!(modes.named(crate::RenderMode::Full), vec!["main"]);
        assert_eq!(modes.named(crate::RenderMode::Line), vec!["peer"]);
        assert!(modes.named(crate::RenderMode::None).is_empty());
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

    /// One walk of the same three roots, in order; not every directory is a skill.
    #[test]
    fn installed_skills_lists_this_homes_skills() {
        let home = dir();
        for name in ["zeta", "alpha"] {
            let dir = home.path().join("skills").join(name);
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(
                dir.join("SKILL.md"),
                format!("---\nname: {name}\ndescription: d\n---\n\nbody\n"),
            )
            .unwrap();
        }
        // No SKILL.md, so not a skill however it is named.
        std::fs::create_dir_all(home.path().join("skills").join("notes")).unwrap();
        // The frontmatter's own name wins over the directory's.
        let dir = home.path().join("skills").join("dir-name");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("SKILL.md"), "---\nname: from-file\n---\n\nbody\n").unwrap();

        let got = installed_skills(home.path(), home.path());
        let mine: Vec<&String> = got
            .iter()
            .filter(|name| matches!(name.as_str(), "alpha" | "zeta" | "from-file"))
            .collect();
        assert_eq!(mine, vec!["alpha", "from-file", "zeta"], "{got:?}");
        assert!(!got.iter().any(|name| name == "notes"), "{got:?}");
    }

    /// A project's own skill is offered before the home's of the same name.
    ///
    /// # Invariant
    ///
    /// The order is the precedence: a checkout that says "here, do it this way"
    /// is more specific than the home that holds it.
    #[test]
    fn a_projects_own_skill_comes_first() {
        let home = dir();
        let project = dir();
        let write = |skills: &Path, name: &str| {
            let dir = skills.join(name);
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join("SKILL.md"), format!("---\nname: {name}\n---\n")).unwrap();
        };
        write(&home.path().join("skills"), "shared");
        write(&project.path().join(".agents/skills"), "shared");
        write(&project.path().join(".agents/skills"), "only-project");

        let got = installed_skills(home.path(), project.path());
        // The machine's skills are in this walk too; ours are the ones checked.
        let mine: Vec<&String> = got
            .iter()
            .filter(|name| matches!(name.as_str(), "shared" | "only-project"))
            .collect();
        assert_eq!(mine, vec!["only-project", "shared", "shared"], "{got:?}");
    }

    /// A mode survives a restart, and the three words it takes are the whole list.
    #[test]
    fn a_render_mode_round_trips() {
        let d = dir();
        assert_eq!(
            RenderModes::load(d.path()).mode_of("main"),
            crate::RenderMode::Full
        );
        let mut modes = RenderModes::load(d.path());
        modes.set("main", crate::RenderMode::Full).unwrap();
        assert_eq!(
            RenderModes::load(d.path()).mode_of("main"),
            crate::RenderMode::Full
        );
        assert_eq!(
            crate::RenderMode::parse(" line "),
            Ok(crate::RenderMode::Line)
        );
        assert_eq!(
            crate::RenderMode::parse("sideways"),
            Err("no render mode \"sideways\"; it takes none, line, full".into())
        );
        // A mode is saved without losing the block size beside it.
        BlockSize(3).save(d.path()).unwrap();
        assert_eq!(BlockSize::load(d.path()), BlockSize(3));
        assert_eq!(
            RenderModes::load(d.path()).mode_of("main"),
            crate::RenderMode::Full
        );
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
