//! The window's own state, in `client.toml` under the client home.
//!
//! See `docs/notes/tui-state.md`.

use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::protocol::{self, ArgSpec, Args, CommandSpec};

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
/// This is the only default: `client.toml` absent, unreadable, or holding rubbish all
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
                protocol::BLOCK_SIZES.join(", ")
            )),
        }
    }

    /// Loads `client_home/client.toml`; a missing or unreadable file caps nothing.
    pub fn load(client_home: &Path) -> Self {
        read_file(&client_home.join("client.toml"))
            .block_size
            .as_deref()
            .and_then(|raw| BlockSize::parse(raw).ok())
            .unwrap_or_default()
    }

    pub fn save(self, client_home: &Path) -> anyhow::Result<()> {
        let path = client_home.join("client.toml");
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
                protocol::BOX_EDGES.join(", ")
            )),
        }
    }

    /// Loads `client_home/client.toml`; a missing file is the default margin.
    pub fn load(client_home: &Path) -> Self {
        read_file(&client_home.join("client.toml"))
            .box_edge
            .as_deref()
            .and_then(|raw| BoxEdge::parse(raw).ok())
            .unwrap_or_default()
    }

    pub fn save(self, client_home: &Path) -> anyhow::Result<()> {
        let path = client_home.join("client.toml");
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
/// set is hidden: a window draws what it was told to draw, and a session it
/// never named is not one of them.
pub struct Shown {
    path: PathBuf,
    show: BTreeMap<String, bool>,
}

impl Shown {
    /// Loads `client_home/client.toml`; a missing file shows nothing.
    pub fn load(client_home: &Path) -> Self {
        let path = client_home.join("client.toml");
        let show = read_file(&path)
            .render_session
            .into_iter()
            .filter_map(|e| stored_show(&e.value).map(|show| (e.name, show)))
            .collect();
        Self { path, show }
    }

    /// Whether this session gets a window beside the list; nobody set is hidden.
    pub fn is_shown(&self, name: &str) -> bool {
        self.show.get(name).copied().unwrap_or(false)
    }

    /// Every name that is shown, in the order the file keeps them.
    pub fn names(&self) -> Vec<String> {
        self.show
            .iter()
            .filter(|(_, show)| **show)
            .map(|(name, _)| name.clone())
            .collect()
    }

    pub fn set(&mut self, name: &str, show: bool) -> anyhow::Result<()> {
        self.show.insert(name.to_string(), show);
        self.save()
    }

    /// Shows one session and hides the rest, in one write, counting what closed.
    ///
    /// # Invariant
    ///
    /// One save for the whole change: a name closed here is a view like any
    /// other, and the file never holds a state the operator never had. The count
    /// is the windows that were open, so the line the operator reads says what
    /// the click did. `known` is which names are sessions now: an entry a deleted
    /// name left is not a window.
    pub fn solo(&mut self, name: &str, known: impl Fn(&str) -> bool) -> anyhow::Result<usize> {
        let closed = self
            .show
            .iter()
            .filter(|(held, show)| **show && held.as_str() != name && known(held))
            .count();
        for (held, show) in self.show.iter_mut() {
            *show = held == name;
        }
        self.show.insert(name.to_string(), true);
        self.save()?;
        Ok(closed)
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

/// `~/.gongga-client/client.toml`.
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

/// The server the window was told to use, as the file spells it.
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

/// The sticky entries the window remembers, keyed by argument *kind*.
pub struct Sticky {
    path: PathBuf,
    args: BTreeMap<String, String>,
}

impl Sticky {
    /// Loads from `client_home/client.toml`; a missing file yields none.
    pub fn load(client_home: &Path) -> Self {
        let path = client_home.join("client.toml");
        let args = read_file(&path)
            .sticky_cmd_arguments
            .into_iter()
            .map(|e| (e.name, e.value))
            .collect();
        Self { path, args }
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

    /// Fills what the caller omitted, from the entries the spec declares.
    ///
    /// # Invariant
    ///
    /// A value lands only on an argument whose `kind` names it. SPEC's two
    /// bullets are one rule: a missing non-optional argument is filled from
    /// its entry, and a missing optional argument takes the entry as its
    /// initial value. An argument no entry names is left alone: absence keeps
    /// the command's own initial, applied where the command runs.
    pub fn fill(&self, spec: Option<&CommandSpec>, args: &mut Args) {
        let Some(spec) = spec else { return };
        for a in &spec.args {
            self.fill_one(a, args);
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

    pub fn names(&self) -> Vec<String> {
        self.args.keys().cloned().collect()
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
}

/// A sticky value an accepted command writes back, by kind.
///
/// # Invariant
///
/// It is a pair, not a command: an entry the server accepted is what the next
/// command inherits, and the window holds it until the `cmd_result` says the
/// command ran.
pub type Follow = Vec<(String, String)>;

/// Everything the window keeps between runs.
pub struct State {
    home: PathBuf,
    client_home: PathBuf,
    sticky: Sticky,
    shown: Shown,
    block_size: BlockSize,
    box_edge: BoxEdge,
}

impl State {
    /// Reads every section of `client_home/client.toml` once.
    ///
    /// # Invariant
    ///
    /// `home` is the harness home and is kept only for what a window *reads* from
    /// it (the skills a completion offers). Nothing here writes to it.
    pub fn load(home: &Path, client_home: &Path) -> Self {
        Self {
            home: home.to_path_buf(),
            client_home: client_home.to_path_buf(),
            sticky: Sticky::load(client_home),
            shown: Shown::load(client_home),
            block_size: BlockSize::load(client_home),
            box_edge: BoxEdge::load(client_home),
        }
    }

    pub fn sticky(&self) -> &Sticky {
        &self.sticky
    }

    /// The names shown beside the list, in the file's order.
    pub fn shown_names(&self) -> Vec<String> {
        self.shown.names()
    }

    pub fn is_shown(&self, name: &str) -> bool {
        self.shown.is_shown(name)
    }

    pub fn block_size(&self) -> BlockSize {
        self.block_size
    }

    pub fn box_edge(&self) -> BoxEdge {
        self.box_edge
    }

    /// The harness home, for what a window reads from it (the skills on offer).
    pub fn home(&self) -> &Path {
        &self.home
    }

    pub fn set_sticky(&mut self, kind: &str, value: &str) -> anyhow::Result<()> {
        self.sticky.set(kind, value)
    }

    /// Writes one entry and answers whether it was a change.
    ///
    /// # Invariant
    ///
    /// "Changed" is a different value, not another setting of the same one: a
    /// line that repeats the model it already had is no reason to forget what
    /// belongs to that model.
    pub fn change_sticky(&mut self, kind: &str, value: &str) -> anyhow::Result<bool> {
        let changed = self.sticky.get(kind) != Some(value);
        self.sticky.set(kind, value)?;
        Ok(changed)
    }

    /// Forgets the entries that follow one that changed -- SPEC's `sticky to xxx`.
    ///
    /// # Invariant
    ///
    /// The dependencies are the published catalog's, passed in, and never a list
    /// here: a value that belongs to a model or a machine is not carried to the
    /// next one, and which values those are is the command's own statement.
    /// Answers the kinds it forgot, so the window can say so.
    pub fn forget_followers(&mut self, changed: &str, follows: &[(String, String)]) -> Vec<String> {
        let mut gone = Vec::new();
        for (kind, parent) in follows {
            if parent != changed || kind == changed || self.sticky.get(kind).is_none() {
                continue;
            }
            if self.sticky.del(kind).unwrap_or(false) {
                gone.push(kind.clone());
            }
        }
        gone
    }

    pub fn del_sticky(&mut self, kind: &str) -> anyhow::Result<bool> {
        self.sticky.del(kind)
    }

    /// Writes one session's mode down, keeping every other section of the file.
    pub fn set_shown(&mut self, name: &str, show: bool) -> anyhow::Result<()> {
        self.shown.set(name, show)
    }

    /// Makes one session the only one shown; answers how many windows closed.
    pub fn show_only(&mut self, name: &str, known: impl Fn(&str) -> bool) -> anyhow::Result<usize> {
        self.shown.solo(name, known)
    }

    pub fn set_block_size(&mut self, size: BlockSize) -> anyhow::Result<()> {
        self.block_size = size;
        size.save(&self.client_home)
    }

    pub fn set_box_edge(&mut self, edge: BoxEdge) -> anyhow::Result<()> {
        self.box_edge = edge;
        edge.save(&self.client_home)
    }

    /// The server the last `connect_server` pointed this window at.
    ///
    /// # Invariant
    ///
    /// Read once, when the window starts: from then on the address in force is
    /// the one this process dials, not the one a file says.
    pub fn connection(&self) -> Option<SocketAddr> {
        let stored = read_file(&self.client_home.join("client.toml")).connection?;
        // Parsed, not formatted: `"::1:9700"` is not an address, and a file
        // that was right must not fall back to the default.
        let ip: std::net::IpAddr = stored.ip.as_deref().unwrap_or("127.0.0.1").parse().ok()?;
        Some(SocketAddr::new(ip, stored.port))
    }

    pub fn remember(&self, addr: SocketAddr) -> anyhow::Result<()> {
        let path = self.client_home.join("client.toml");
        let mut file = read_file(&path);
        file.connection = Some(Stored {
            ip: Some(addr.ip().to_string()),
            port: addr.port(),
        });
        write_file(&path, &file)
    }
}

/// Copies a `client.toml` from the harness home into a client home that has none.
///
/// # Invariant
///
/// The file used to live in the harness home, so a home that predates the move
/// keeps what the operator set: the copy is what later writes keep, and the
/// original is left in place for a build that still reads it. Copying happens
/// only when the destination is absent, so a second window never rewinds a
/// first one's settings.
pub fn adopt_legacy(home: &Path, client_home: &Path) {
    let old = home.join("client.toml");
    let new = client_home.join("client.toml");
    if new.exists() || !old.is_file() {
        return;
    }
    if std::fs::create_dir_all(client_home).is_ok() {
        let _ = std::fs::copy(&old, &new);
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
