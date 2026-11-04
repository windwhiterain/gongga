//! Sticky arguments and shown windows: the client's own state in `cli.toml`.
//!
//! See `docs/notes/cli-commands.md`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use protocol::{ArgSpec, Args, BLOCK_SIZES, BOX_EDGES, CommandSpec};
use serde::{Deserialize, Serialize};

/// How many wrapped rows one block may fill. Client-side only.
///
/// # Invariant
///
/// `0` caps nothing, and it is the only way to say so: a front-end never has to
/// recognise a sentinel mode or a magic mode name. Any other number is a cap, and
/// a fresh home starts at [`DEFAULT_BLOCK_SIZE`] -- SPEC's `initial: 8`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BlockSize(pub usize);

/// What `set_render --block_size` is before anyone says otherwise.
///
/// # Invariant
///
/// This is the only default: `cli.toml` absent, unreadable, or holding rubbish all
/// answer this, so a home never starts with a cap nobody typed.
pub const DEFAULT_BLOCK_SIZE: usize = 8;

impl Default for BlockSize {
    fn default() -> Self {
        Self(DEFAULT_BLOCK_SIZE)
    }
}

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

/// The columns a box leaves to its pane on each side. Client-side only.
///
/// # Invariant
///
/// A width, never a mode: `0` is a box that touches both edges of its window, and
/// every other number is that many columns on the left and as many on the right.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BoxEdge(pub usize);

impl Default for BoxEdge {
    /// SPEC's starting margin: a box drawn edge to edge leaves nothing to grab.
    fn default() -> Self {
        Self(protocol::DEFAULT_BOX_EDGE)
    }
}

impl BoxEdge {
    /// A width, or the way to name one.
    pub fn parse(raw: &str) -> Result<Self, String> {
        match raw.trim().parse::<usize>() {
            Ok(columns) => Ok(Self(columns)),
            Err(_) => Err(format!(
                "no box edge {raw:?}: the columns a box leaves to its pane on each \
                 side; widths worth typing are {}",
                BOX_EDGES.join(", ")
            )),
        }
    }

    /// Loads `home/cli.toml`; a missing or unreadable file is the default margin.
    pub fn load(home: &Path) -> Self {
        read_file(&home.join("cli.toml"))
            .box_edge
            .as_deref()
            .and_then(|raw| BoxEdge::parse(raw).ok())
            .unwrap_or_default()
    }

    pub fn save(self, home: &Path) -> anyhow::Result<()> {
        let path = home.join("cli.toml");
        let mut file = read_file(&path);
        file.box_edge = Some(self.0.to_string());
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
    /// `set_render --box_edge`: the columns a box leaves to its pane on each side.
    #[serde(default)]
    box_edge: Option<String>,
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

/// The sticky entries one client remembers, keyed by argument *kind* -- the
/// `<session>`, `<model>` types the published catalog declares, never by an
/// argument's name.
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

    /// `kind` -> `value` pairs, for the status bar.
    pub fn pairs(&self) -> Vec<(String, String)> {
        self.args
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect()
    }

    pub fn get(&self, kind: &str) -> Option<&str> {
        self.args.get(kind).map(String::as_str)
    }

    pub fn set(&mut self, kind: &str, value: &str) -> anyhow::Result<()> {
        self.args.insert(kind.to_string(), value.to_string());
        self.save()
    }

    /// Returns false when there was nothing to remove.
    pub fn del(&mut self, kind: &str) -> anyhow::Result<bool> {
        match self.args.remove(kind) {
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

    /// Fills what the caller omitted, from the entries the published spec
    /// declares -- the spec decides which entries exist, and a value only
    /// lands on an argument whose `kind` names it.
    ///
    /// # Invariant
    ///
    /// SPEC's Sticky section. A bundle is judged as a unit: partly typed,
    /// `Sticky` leaves the rest of it alone (all or nothing); wholly omitted,
    /// every member an entry holds fills -- an incomplete bundle is no bundle,
    /// the members fill one by one. Everything else with a kind fills when
    /// omitted; absence where no entry names the argument is a decision, left
    /// alone.
    pub fn fill(&self, spec: Option<&CommandSpec>, args: &mut Args) {
        let Some(spec) = spec else { return };
        let mut bundles: Vec<(&str, Vec<&ArgSpec>)> = Vec::new();
        for a in &spec.args {
            let Some(bundle) = a.bundle.as_deref() else {
                continue;
            };
            match bundles.iter_mut().find(|(name, _)| *name == bundle) {
                Some((_, members)) => members.push(a),
                None => bundles.push((bundle, vec![a])),
            }
        }
        for (_, members) in &bundles {
            let typed = members
                .iter()
                .filter(|m| args.contains_key(&m.name))
                .count();
            if typed > 0 && typed < members.len() {
                continue;
            }
            for m in members {
                self.fill_one(m, args);
            }
        }
        for a in &spec.args {
            if a.bundle.is_none() {
                self.fill_one(a, args);
            }
        }
    }

    /// One argument, one entry: fill when omitted and an entry names its kind.
    fn fill_one(&self, a: &ArgSpec, args: &mut Args) {
        if args.contains_key(&a.name) {
            return;
        }
        let Some(kind) = a.kind.as_deref() else {
            return;
        };
        if let Some(v) = self.args.get(kind) {
            args.insert(a.name.clone(), v.clone());
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

/// The skills a session can open: the project's own, the home's, then the machine's.
///
/// # Invariant
///
/// One walk, shared with the server's `index_skill` and with `skill <name>` -- so a
/// name read here is a name that can be opened. `project` is the directory the session
/// works in, and `None` when that directory is another machine's: its checkout is not
/// on this disk. See `docs/notes/cli-commands.md`.
pub fn installed_skills(home: &Path, project: Option<&Path>) -> Vec<String> {
    roots(home, project)
        .all()
        .into_iter()
        .map(|skill| skill.name)
        .collect()
}

/// The three roots one session reads a skill from, as this machine can see them.
pub fn roots(home: &Path, project: Option<&Path>) -> skill::Roots {
    skill::Roots::new(
        project.map(|dir| dir.join(".agents").join("skills")),
        home.join("skills"),
        dirs::home_dir().map(|home| home.join(".agents").join("skills")),
    )
}

/// Binds a line's tokens to a published spec: bare words fill the orderable
/// slots; `--flag value` names the rest.
///
/// # Invariant
///
/// The spec is the only syntax the caller knows: a command this client never
/// heard of cannot be parsed, and a token that names nothing in the spec is a
/// typo to report, not a shape to guess at. A value may start with `--` only
/// in the `--flag=--value` spelling; a bare `--word` is read as a flag. Every
/// argument a `--flag` names binds by that flag; a bare word binds the next
/// unfilled argument that may be spelled by order -- a positional, or a
/// required one (SPEC: "all non optional named argument's name can be ommited
/// via argument order"). An optional argument keeps its flag.
pub fn parse_args(tokens: &[String], spec: Option<&CommandSpec>) -> anyhow::Result<Args> {
    let flags: Vec<&ArgSpec> = spec.map(|s| s.args.iter().collect()).unwrap_or_default();
    let slots: Vec<&ArgSpec> = spec
        .map(|s| {
            s.args
                .iter()
                .filter(|a| a.positional || a.required)
                .collect()
        })
        .unwrap_or_default();
    let takes = |name: &str| flags.iter().any(|a| a.name == name);
    let mut args = Args::new();
    let mut next_slot = 0usize;
    let mut i = 0;
    while i < tokens.len() {
        let token = &tokens[i];
        if token.starts_with("--") {
            let flag = token.trim_start_matches('-');
            if let Some((name, value)) = flag.split_once('=') {
                if !takes(name) {
                    anyhow::bail!(
                        "{} has no argument --{name}; it takes {}",
                        spec.map(|s| s.name.as_str()).unwrap_or("this command"),
                        signature(spec),
                    );
                }
                insert_once(&mut args, name, value)?;
                i += 1;
            } else {
                if !takes(flag) {
                    anyhow::bail!(
                        "{} has no argument --{flag}; it takes {}",
                        spec.map(|s| s.name.as_str()).unwrap_or("this command"),
                        signature(spec),
                    );
                }
                let value = tokens
                    .get(i + 1)
                    .ok_or_else(|| anyhow::anyhow!("--{flag} needs a value"))?;
                insert_once(&mut args, flag, value)?;
                i += 2;
            }
        } else {
            let Some(slot) = slots.get(next_slot) else {
                anyhow::bail!(
                    "unexpected argument {token:?}; {} takes {}",
                    spec.map(|s| s.name.as_str()).unwrap_or("this command"),
                    signature(spec),
                );
            };
            insert_once(&mut args, &slot.name, token)?;
            next_slot += 1;
            i += 1;
        }
    }
    Ok(args)
}

/// A repeated argument is a contradiction, whatever spellings it was given.
fn insert_once(args: &mut Args, name: &str, value: &str) -> anyhow::Result<()> {
    if args.contains_key(name) {
        anyhow::bail!("argument --{name} given twice");
    }
    args.insert(name.to_string(), value.to_string());
    Ok(())
}

/// The one-line syntax of a command: `<hint>`, `[<hint>]`, `--name <hint>`.
pub fn signature(spec: Option<&CommandSpec>) -> String {
    let Some(spec) = spec else {
        return "nothing".to_string();
    };
    if spec.args.is_empty() {
        return "nothing".to_string();
    }
    spec.args
        .iter()
        .map(|a| {
            let hint = a.value_hint.as_deref().unwrap_or(&a.name);
            if a.positional {
                if a.required {
                    format!("<{hint}>")
                } else {
                    format!("[<{hint}>]")
                }
            } else if a.required {
                format!("--{} <{}>", a.name, hint)
            } else {
                format!("[--{} <{}>]", a.name, hint)
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// Renders bound arguments back to a command line, positionals bare in their
/// published order, for the echoed command row.
pub fn render_args(args: &Args, spec: Option<&CommandSpec>) -> String {
    let mut out = String::new();
    if let Some(spec) = spec {
        for arg in spec.args.iter().filter(|a| a.positional) {
            if let Some(value) = args.get(&arg.name) {
                out.push(' ');
                out.push_str(&render_value(value));
            }
        }
    }
    for (k, v) in args.iter() {
        if spec
            .map(|s| s.args.iter().any(|a| a.positional && a.name == *k))
            .unwrap_or(false)
        {
            continue;
        }
        out.push_str(&format!(" --{k} {}", render_value(v)));
    }
    out
}

/// One value as it reads on a command line: quoted when it holds a space.
fn render_value(value: &str) -> String {
    if value.contains(' ') {
        format!("\"{value}\"")
    } else {
        value.to_string()
    }
}
