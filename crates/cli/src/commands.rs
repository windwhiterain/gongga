//! Sticky arguments and shown windows: the client's own state in `cli.toml`.
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

/// Which sessions are shown: `set_render_session --show`.
///
/// # Invariant
///
/// It is a view, never a deletion: a session left hidden keeps every block it ever
/// carried, so showing it again brings the whole history back. A session nobody
/// set is shown: a window has to show something.
pub struct Shown {
    path: PathBuf,
    show: BTreeMap<String, bool>,
}

impl Shown {
    /// Loads `home/cli.toml`; a missing or unreadable file shows everything.
    pub fn load(home: &Path) -> Self {
        let path = home.join("cli.toml");
        let show = read_file(&path)
            .render_session
            .into_iter()
            .filter_map(|e| stored_show(&e.value).map(|show| (e.name, show)))
            .collect();
        Self { path, show }
    }

    /// Whether this session gets a window beside the list; nobody set is shown.
    pub fn is_shown(&self, name: &str) -> bool {
        self.show.get(name).copied().unwrap_or(true)
    }

    pub fn set(&mut self, name: &str, show: bool) -> anyhow::Result<()> {
        self.show.insert(name.to_string(), show);
        self.save()
    }

    fn save(&self) -> anyhow::Result<()> {
        let mut file = read_file(&self.path);
        file.render_session = self
            .show
            .iter()
            .map(|(name, show)| Entry {
                name: name.clone(),
                value: show.to_string(),
            })
            .collect();
        write_file(&self.path, &file)
    }
}

/// `--show` as a value, or what is wrong with it: never a silent default.
pub fn parse_show(raw: &str) -> Result<bool, String> {
    match raw.trim() {
        "true" => Ok(true),
        "false" => Ok(false),
        other => Err(format!(
            "no show value {other:?}; it takes {}",
            protocol::SHOW_VALUES.join(", ")
        )),
    }
}

/// A stored value as a show state, including the three words a mode once spelled.
fn stored_show(raw: &str) -> Option<bool> {
    match raw.trim() {
        "true" | "full" | "line" => Some(true),
        "false" | "none" => Some(false),
        _ => None,
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

    /// Fills in an argument the caller omitted, when a sticky value stands for it.
    ///
    /// # Invariant
    ///
    /// Only a required argument, or one the spec declares `sticky`, is filled --
    /// SPEC spells those `(sticky)`. Every other optional argument is left alone,
    /// because absence there is a decision: a filter that keeps what it had must
    /// not be overwritten by a value meant for a different command.
    pub fn fill(&self, spec: Option<&CommandSpec>, args: &mut Args) {
        let Some(spec) = spec else { return };
        for a in &spec.args {
            if (a.required || a.sticky) && !args.contains_key(&a.name) {
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
