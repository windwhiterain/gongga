//! `gongga-cli`: the command layer.
//!
//! See `docs/notes/cli-lib.md`.

pub mod catalog;
pub mod commands;
pub mod complete;
pub mod server;
pub mod skill_command;

pub use crate::catalog::Catalog;
pub use crate::commands::{BlockSize, BoxEdge, Shown, Sticky};
pub use crate::complete::{Candidate, Completer, Completion, SharedCompleter, token_bounds};
pub use crate::server::{ensure, spawn, starts_here, stop};
pub use protocol;

use std::collections::{HashMap, HashSet};

use std::net::{SocketAddr, ToSocketAddrs};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, anyhow};
use clap::{CommandFactory, FromArgMatches, Parser};
use protocol::{
    ChannelConfig, ClientInfo, ClientMessage, Event, LogLevel, ResponseEvent, ResponsePart,
    ServerMessage, SessionInfo, SessionStatus, ToolAnswerEvent, decode_server, encode_client,
};
use tokio::net::UdpSocket;

use crate::commands::{parse_args, render_args};

const DEFAULT_SERVER: &str = "127.0.0.1:9700";
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(5);

// Where to connect. Not a doc comment: clap would fold it into the about line.
#[derive(Debug, Clone, clap::Args)]
pub struct Connection {
    /// Server address, e.g. `127.0.0.1:9700`.
    #[arg(long, global = true)]
    pub server: Option<String>,
    /// Harness home holding config.toml and cli.toml. Defaults to `~/.gongga`.
    #[arg(long, global = true)]
    pub home: Option<PathBuf>,
    /// This client's own directory: its events and its scrollback, never the
    /// server's. Defaults to `~/.gongga-client`.
    #[arg(long, global = true)]
    pub client_home: Option<PathBuf>,
    /// Connection key. Defaults to `[server] key` in ~/.gongga/config.toml.
    #[arg(long, global = true)]
    pub key: Option<String>,
}

impl Connection {
    /// Where to dial and with which key, resolved once per process.
    pub fn endpoint(&self) -> anyhow::Result<Endpoint> {
        let addr = resolve_server(self)?;
        let key = resolve_key(self)
            .ok_or_else(|| anyhow!("no connection key: pass --key or set GONGGA_KEY"))?;
        Ok(Endpoint { addr, key })
    }
}

#[derive(Debug, Parser)]
#[command(name = "gongga-cli")]
pub struct Args {
    #[command(flatten)]
    pub conn: Connection,
    /// Never start a server for this client; the window waits instead.
    ///
    /// # Invariant
    ///
    /// The name SPEC gives it, `--client_only`, with the hyphenated spelling
    /// accepted as an alias: a flag the published spec spells one way and the
    /// binary another is a flag the reader cannot type.
    #[arg(long = "client_only", alias = "client-only", global = true)]
    pub client_only: bool,
    #[command(subcommand)]
    pub command: Option<Sub>,
}

impl Args {
    /// Rejects a global flag written after the command name, where clap would
    /// hide it.
    ///
    /// # Invariant
    ///
    /// `serve --home` is the one exception: that command's subject *is* a home,
    /// so `gongga serve --home <dir>` reads as one sentence. Every other command
    /// takes the flag before its name, where clap can see it.
    pub fn check_flag_order(&self) -> anyhow::Result<()> {
        let Some(Sub::Cmd(command)) = &self.command else {
            return Ok(());
        };
        let serves = command.first().map(String::as_str) == Some("serve");
        // A skill's argv is the entry's: `skill remote add --home x` is the script's flag.
        if command.first().map(String::as_str) == Some("skill") {
            return Ok(());
        }
        // Every command flag takes one value, which may itself look like a flag:
        // walk pairs rather than match every token.
        let mut i = 1;
        while i < command.len() {
            let token = &command[i];
            let ordered = [
                "--server",
                "--home",
                "--client-home",
                "--key",
                "--client_only",
                "--client-only",
            ];
            if let Some(flag) = ordered
                .iter()
                .find(|flag| token.as_str() == **flag || token.starts_with(&format!("{flag}=")))
                .filter(|flag| !(serves && **flag == "--home"))
            {
                anyhow::bail!(
                    "{flag} has to come before the command: `gongga {flag} <value> {} ...`",
                    command[0]
                );
            }
            i += if token.starts_with("--") && !token.contains('=') {
                2
            } else {
                1
            };
        }
        Ok(())
    }
}

#[derive(Debug, clap::Subcommand)]
pub enum Sub {
    /// The command itself, unknowable until the handshake; one per process.
    #[command(external_subcommand)]
    Cmd(Vec<String>),
}

/// Parses the command line, one way for both binaries, each naming itself.
pub fn parse_argv(name: &'static str, about: &'static str) -> Args {
    // `bin_name` is the word clap prints in `Usage:`; without it a bundle's help
    // answers as `gongga-cli.exe`.
    let matches = Args::command()
        .name(name)
        .bin_name(name)
        .about(about)
        .version(protocol::version_line())
        .get_matches();
    Args::from_arg_matches(&matches).expect("clap already rejected what it could not parse")
}

/// The subcommand a program that *carries* the command layer answers to for it.
///
/// See `docs/notes/tui-main.md`.
pub const CLI: &str = "cli";

/// The name a shell types, and the name this program answers to in a home's `bin`.
///
/// # Invariant
///
/// The crate's own name, not a short form: one name for the package, the program
/// and the link.
pub const COMMAND: &str = "gongga-cli";

/// The name this process was started as, which is what a reader can type back.
///
/// # Invariant
///
/// argv[0] rather than `COMMAND`, because the same code is also the `gongga` a
/// bundle carries (`bundle-main.md`): a message that tells someone to run
/// `gongga-cli` is wrong when the file on their disk is called `gongga`, and the
/// name is the only thing that knows which it is.
pub fn typed_name() -> String {
    // A bundle hands over to an extracted program, so argv[0] is that file; it
    // passes its own name in this variable.
    if let Some(named) = std::env::var("GONGGA_NAME")
        .ok()
        .map(|name| name.trim().to_string())
        .filter(|name| !name.is_empty())
    {
        return named;
    }
    std::env::args_os()
        .next()
        .map(std::path::PathBuf::from)
        .and_then(|path| {
            path.file_stem()
                .map(|stem| stem.to_string_lossy().into_owned())
        })
        .filter(|stem| !stem.is_empty())
        .unwrap_or_else(|| COMMAND.to_string())
}

/// The name to answer `--help` with: what was typed, or this program's own.
///
/// # Invariant
///
/// `gongga-cli` is right when the file is called that, and wrong inside a bundle,
/// where the words typed were `gongga cli`. clap wants a `'static` str and this
/// runs once, so the leak is the whole of the cost.
pub fn spoken_name() -> &'static str {
    let name = std::env::var("GONGGA_NAME")
        .ok()
        .map(|name| name.trim().to_string())
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| COMMAND.to_string());
    Box::leak(name.into_boxed_str())
}

/// What this program says it is, wherever it is asked -- one layer, one sentence.
pub const ABOUT: &str = "Gongga harness commands: one command in, its events out";

/// The home this process works on: `--home`, then `GONGGA_HOME`, then `~/.gongga`.
pub fn home_of(args: &Args) -> std::path::PathBuf {
    home_dir(args.conn.home.as_deref())
}

/// Where this client keeps what it drew: `--client-home`, then the variable, then
/// `~/.gongga-client`.
///
/// # Invariant
///
/// Never the server's home and never derived from the greeting: a window's events
/// are this process's own record, so the place they live is this process's choice.
/// See `docs/notes/tui-cache.md`.
pub fn client_home_of(args: &Args) -> std::path::PathBuf {
    args.conn
        .client_home
        .clone()
        .or_else(|| std::env::var_os("GONGGA_CLIENT_HOME").map(std::path::PathBuf::from))
        .or_else(|| dirs::home_dir().map(|home| home.join(".gongga-client")))
        .unwrap_or_else(|| std::path::PathBuf::from(".gongga-client"))
}

/// The address this client would dial, as a name for the records about it.
///
/// # Invariant
///
/// From the client's own configuration, not from a greeting: two servers are two
/// records, and which one a window is talking to is known before it connects.
pub fn endpoint_name(conn: &Connection) -> String {
    match resolve_server(conn) {
        Ok(addr) => addr.to_string(),
        Err(_) => "unresolved".to_string(),
    }
}

// Where rendered output goes

/// A head's folded row: SPEC's `abstract`, with the space the mark leaves room for.
///
/// # Invariant
///
/// One place spells this, so the block a call opens with and the label its outcome
/// later replaces cannot differ by a space.
fn head_label(head: &protocol::Head) -> String {
    let text = head.summary();
    match text.is_empty() {
        true => String::new(),
        false => format!("{text} "),
    }
}

/// Which line kind a server-named [`protocol::HeadKind`] is drawn as.
///
/// # Invariant
///
/// The mapping is a *style* decision, so it lives here and not on the wire: the
/// server says a call failed, and this crate decides that reads as a tool error
/// rather than as an error of the harness. A new kind is one arm here and one
/// look in the renderer, with no protocol change.
fn kind_of(kind: protocol::HeadKind) -> LineKind {
    match kind {
        protocol::HeadKind::Call => LineKind::ToolCall,
        protocol::HeadKind::Ok => LineKind::ToolOutput,
        protocol::HeadKind::Failed => LineKind::ToolError,
        protocol::HeadKind::Reasoning => LineKind::Reasoning,
        protocol::HeadKind::Output => LineKind::Assistant,
        protocol::HeadKind::Cmd => LineKind::Command,
        protocol::HeadKind::Error => LineKind::Error,
    }
}

impl LineKind {
    /// The one column an item of this kind is marked with.
    ///
    /// # Invariant
    ///
    /// SPEC gives `abstract`, `cmd` and `error` a coloured symbol and gives `output`
    /// none: a mark says "this is an item", so the one kind that is the answer
    /// itself carries no mark. Chosen here, never sent: the server says what a row
    /// *is*, and this crate says what that looks like.
    pub fn symbol(self) -> &'static str {
        match self {
            // SPEC names no item for these: the harness's remarks, the prose the
            // operator typed, and the answer carry no mark.
            LineKind::System | LineKind::User | LineKind::Assistant => "",
            LineKind::Command => "$",
            LineKind::Reasoning => "\u{25c7}",
            LineKind::ToolCall => "\u{25b8}",
            LineKind::ToolOutput => "\u{2713}",
            LineKind::ToolError => "\u{2715}",
            LineKind::Result => "\u{2192}",
            LineKind::Error => "!",
        }
    }

    /// The glyph and the space in front of the item's own words.
    ///
    /// # Invariant
    ///
    /// The one column SPEC colours: everything after it is the model's text, and the
    /// window draws that in its own ink rather than in this kind's.
    pub fn mark(self) -> String {
        match self.symbol() {
            "" => String::new(),
            symbol => format!("{symbol} "),
        }
    }

    /// Whether an item of this kind is SPEC's `abstract`: a call, its result, or the
    /// reasoning behind it.
    pub fn folds(self) -> bool {
        matches!(
            self,
            LineKind::Reasoning | LineKind::ToolCall | LineKind::ToolOutput | LineKind::ToolError
        )
    }
}

/// What a rendered line is; the front-end only colours it.
///
/// # Invariant
///
/// A closed set, so it may be written down: a window's own cache stores the kind a
/// block was drawn as, and a build that meets a kind it cannot name refuses the
/// record rather than guessing a colour. See `docs/notes/tui-cache.md`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LineKind {
    /// The harness talking about itself: connected, offline, a status word.
    System,
    /// A command line, echoed: `/cmd` as typed, or `cmd --args` as it ran.
    ///
    /// # Invariant
    ///
    /// Its own kind, not `System`: the question and the model's thinking are two
    /// different things to read, and a colour that says "something happened" for
    /// both is a colour that says nothing.
    Command,
    User,
    Assistant,
    Reasoning,
    ToolCall,
    ToolOutput,
    ToolError,
    Result,
    Error,
}

/// One session in the list the window draws on the left.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Listed {
    pub name: String,
    /// The status word, spelled: `idle`, `running`, `error`. A front-end that
    /// knows no protocol type can still draw it.
    pub status: String,
    /// SPEC's `full` pane title: the measurements, then what the session runs on.
    pub title: String,
}

/// Everything the window needs to lay itself out, as SPEC's UI section has it.
///
/// # Invariant
///
/// One decision read two ways: SPEC's session list is every session's name, and
/// `shown` is the ones that also get a header and their events beside it. A name
/// appears in both, because the list is where a session is clicked -- it is not a
/// list of what is hidden.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Layout {
    /// Every session the server knows, in its order.
    pub listed: Vec<Listed>,
    /// Sessions drawn with a header and their events, in the list's order.
    pub shown: Vec<String>,
    /// The window `cmd` and `cmd_result` are drawn in.
    pub command: String,
    /// SPEC's `active_quota` row, when the last step measured one.
    pub quota: Option<QuotaRow>,
}

/// SPEC's `active_quota` row, spelled once for every front-end.
///
/// # Invariant
///
/// Only the numbers the provider reported appear, in SPEC's own units, and a row
/// with no figures says why: a balance nobody could read is not a zero balance.
/// See `docs/notes/cli-lib.md`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QuotaRow {
    pub text: String,
    /// Where the row is drawn: a fact, or the reason there is no fact.
    pub kind: LineKind,
}

/// What the command line's window is called.
pub const COMMAND_WINDOW: &str = "cmd";

/// Everything the client can ask a front-end to show; neither implementation
/// knows what produced the text.
pub trait Sink {
    /// A finished line, in the window `window` names.
    fn line(&mut self, window: &str, kind: LineKind, text: &str);
    /// A streamed block, in the window `window` names.
    fn stream(
        &mut self,
        window: &str,
        key: &str,
        kind: LineKind,
        prefix: &str,
        text: &str,
        done: bool,
    );
    fn status(&mut self, text: &str);
    /// The harness's own news, never a command's line.
    fn note(&mut self, _kind: LineKind, _text: &str) {}
    /// How tall one block may get, in wrapped rows; 0 caps nothing. A front-end
    /// that shows every row ignores it.
    fn block_size(&mut self, _lines: usize) {}
    /// The columns a box leaves to its pane on each side. A front-end that draws no
    /// boxes ignores it.
    fn box_edge(&mut self, _columns: usize) {}
    /// How the window lays itself out. A front-end that ignores it shows one list.
    fn layout(&mut self, _layout: &Layout) {}
    /// Replaces the whole text of a block: a call's structure, once known.
    fn rewrite(&mut self, _window: &str, _key: &str, _text: &str) {}
    /// Describes a call with structured fields for a front-end that shows them.
    /// Other front-ends keep the detail text.
    fn tool_call(&mut self, window: &str, key: &str, head: &protocol::Head, streamed: bool) {
        if streamed {
            self.rewrite(window, key, &head.detail());
            self.relabel(window, key, &head_label(head), kind_of(head.kind));
        } else {
            self.stream(
                window,
                key,
                kind_of(head.kind),
                &head_label(head),
                &head.detail(),
                true,
            );
        }
    }
    /// The label of a block that learned its outcome after opening, and that
    /// label's kind: SPEC's `tool_result{ok, time}`.
    ///
    /// # Invariant
    ///
    /// The text is the server's and the kind is the server's; the look of that
    /// kind is the front-end's, which is why the kind travels rather than a colour.
    fn relabel(&mut self, _window: &str, _key: &str, _tag: &str, _kind: LineKind) {}
    /// Empties the window `window` names: the session that wrote it is deleted.
    fn forget(&mut self, _window: &str) {}
    /// Drops what a window holds, keeping it: SPEC's command row shows the last
    /// `cmd` or `cmd_result`, not their history.
    fn clear(&mut self, _window: &str) {}
    /// The session the sticky `session` names: what the keys scroll and what a
    /// click sets. See `docs/notes/cli-lib.md`.
    fn sticky_session(&mut self, _name: Option<&str>) {}
    /// A session stopped: its turn ended, and it waits for the operator.
    ///
    /// # Invariant
    ///
    /// Told once per turn that ended, and never for a turn that is still
    /// running: a front-end that shows it and one that ignores it hear the same
    /// fact. See `docs/notes/cli-lib.md`.
    fn stopped(&mut self, _session: &str, _failed: bool) {}
}

/// The one-shot mode's sink: it prints nothing, but it remembers what went
/// wrong.
#[derive(Default)]
struct NullSink {
    errors: Vec<String>,
}

impl NullSink {
    fn take_error(&mut self) -> Option<String> {
        self.errors.pop()
    }
}

impl Sink for NullSink {
    fn line(&mut self, _window: &str, kind: LineKind, text: &str) {
        if matches!(kind, LineKind::Error) {
            self.errors.push(text.to_string());
        }
    }
    /// The one-shot mode has no window: a note goes to stderr, so a pipe of JSON
    /// keeps carrying exactly the events.
    fn note(&mut self, kind: LineKind, text: &str) {
        eprintln!("{text}");
        if matches!(kind, LineKind::Error) {
            self.errors.push(text.to_string());
        }
    }
    fn stream(
        &mut self,
        _window: &str,
        _key: &str,
        _kind: LineKind,
        _prefix: &str,
        _text: &str,
        _done: bool,
    ) {
    }
    fn status(&mut self, _text: &str) {}
}

/// What an input line turned into.
pub enum Submission {
    /// Goes to the server.
    Send(ClientMessage),
    /// Handled locally: done, nothing to send.
    Local,
    /// Rejected, with the reason already reported through the sink.
    Rejected,
    /// Nothing was sent because there is no link; the line itself is valid.
    Offline,
}

// Client state

pub struct Client {
    catalog: Catalog,
    sticky: Sticky,
    /// How tall one block may get, in wrapped rows.
    block_size: BlockSize,
    /// The columns a box leaves to its pane on each side.
    box_edge: BoxEdge,
    /// Which sessions are shown beside the list.
    shown: Shown,
    /// Calls that streamed their arguments, by id: their structure fills that block.
    calls: HashSet<String>,
    /// Which window each accepted command's result belongs to, from its echo.
    results: HashMap<u64, String>,
    /// A `set_session` waiting for its acknowledgement -- the sticky entries an
    /// accepted one writes back, as (kind, value) pairs, so a refused one
    /// writes nothing. See `docs/notes/cli-lib.md`.
    follows: Vec<(u64, Vec<(String, String)>)>,
    sessions: Vec<SessionInfo>,
    model: String,
    next_id: u64,
    /// Commands the completion sent, by the remote each asked: a popup, not a line.
    quiet: HashMap<u64, String>,
    /// Whether a link is open right now; while false, all server knowledge is
    /// stale.
    connected: bool,
    /// Failed dials since the link went down, so silence is legible.
    attempts: u32,
    /// Set by `connect_server`; the front-end reads it and dials again.
    server_changed: bool,
    /// The server said it is finishing a `cancel`. See `docs/notes/cli-lib.md`.
    draining: bool,
    /// Another build or home: part of the facts, which no note can wipe.
    build_note: Option<String>,
    home_note: Option<String>,
    /// SPEC's `active_quota` row, as the last step's reading spelled it.
    quota: Option<QuotaRow>,
}

/// What the operator typed, for the purpose of echoing it back into the log.
#[derive(Clone, Copy)]
enum Echo<'a> {
    /// A `/command`, echoed as typed.
    Command,
    /// Prose, echoed as prose -- and coloured as the user's turn, so a reply
    /// has the question above it.
    Message(&'a str),
}

impl Client {
    /// Echoes what the operator typed into the window its command named.
    /// Echoes the line where its answer will be read.
    ///
    /// # Invariant
    ///
    /// A command that acts on this window answers on the command line, so its echo
    /// goes there too: an answer without its question reads as a line of somebody
    /// else's transcript that landed in the wrong place.
    fn show(&self, sink: &mut dyn Sink, echo: &Echo<'_>, tokens: &[String], session: Option<&str>) {
        let window = self.command_line(session);
        match echo {
            Echo::Command => sink.line(
                &window,
                LineKind::Command,
                &format!("/{}", tokens.join(" ")),
            ),
            Echo::Message(text) => sink.line(&window, LineKind::User, text),
        }
    }

    /// Writes the command row, replacing what it held.
    ///
    /// # Invariant
    ///
    /// SPEC's row is `last cmd | cmd_result`: it shows the last thing the operator
    /// asked or was told, not a log. So every client-side answer replaces the one
    /// before it -- a `set_render` twice is one row, not two. A line that belongs
    /// to a session is written to that session's window and keeps its history.
    fn row(&self, sink: &mut dyn Sink, kind: LineKind, text: &str) {
        let window = self.command_window();
        sink.clear(&window);
        sink.line(&window, kind, text);
    }

    /// The window for a line that names no session, named so no session can take it.
    fn command_window(&self) -> String {
        let mut name = COMMAND_WINDOW.to_string();
        while self.sessions.iter().any(|s| s.name == name) {
            name.push(' ');
        }
        name
    }

    /// Where a command's echo and its result are read: a command that named a
    /// session belongs with that session's own lines.
    fn command_line(&self, session: Option<&str>) -> String {
        match session {
            Some(name) => name.to_string(),
            None => self.command_window(),
        }
    }

    /// Where an answer may still be read: its own window while the client draws it,
    /// and the command line otherwise.
    ///
    /// # Invariant
    ///
    /// A command that deletes its session is answered after the session left the
    /// list, so writing there would leave a line under a name the server hands out
    /// again -- and nobody would see it, because no pane draws that name any more.
    /// See `docs/notes/cli-lib.md`.
    fn answer_window(&self, window: String) -> String {
        let live = window == self.command_window()
            || self.sessions.iter().any(|session| session.name == window);
        if live { window } else { self.command_window() }
    }

    /// What a block opens with: the label says which part of the answer it is.
    ///
    /// # Invariant
    ///
    /// The session's name is never in the text: the window draws it, bracketed, in
    /// the list and in its own header. Two places saying it is how `[main] [main]
    /// ...` happened. See `docs/notes/cli-lib.md`.
    fn tag(&self, _session: &str, label: &str) -> String {
        label.to_string()
    }

    /// How the window lays itself out: SPEC's UI section, as its lists.
    ///
    /// # Invariant
    ///
    /// Every session the server knows is listed whatever its window -- the list is
    /// where a session is clicked -- and `shown` is the ones drawn beside it.
    fn layout(&self) -> Layout {
        Layout {
            listed: self
                .sessions
                .iter()
                .map(|s| Listed {
                    name: s.name.clone(),
                    status: match s.status {
                        SessionStatus::Idle => "idle",
                        SessionStatus::Running => "running",
                        SessionStatus::Error => "error",
                    }
                    .to_string(),
                    title: title_note(s),
                })
                .collect(),
            // A session nobody set is shown, so this reads the sessions rather
            // than the file's entries.
            shown: self
                .sessions
                .iter()
                .filter(|s| self.shown.is_shown(&s.name))
                .map(|s| s.name.clone())
                .collect(),
            command: self.command_window(),
            quota: self.quota.clone(),
        }
    }

    fn new(catalog: Catalog, sticky: Sticky) -> Self {
        let block_size = BlockSize::load(sticky.home());
        let box_edge = BoxEdge::load(sticky.home());
        let shown = Shown::load(sticky.home());
        catalog.set_sticky_names(&sticky.names());
        // Where relative values are read until a session says: this process's,
        // which the greeting replaces with the server's.
        catalog.set_roots(sticky.home(), &std::env::current_dir().unwrap_or_default());
        Self {
            catalog,
            sticky,
            block_size,
            box_edge,
            shown,
            results: HashMap::new(),
            follows: Vec::new(),
            calls: HashSet::new(),
            sessions: Vec::new(),
            model: "-".to_string(),
            next_id: Self::seed_command_id(),
            quiet: HashMap::new(),
            connected: false,
            attempts: 0,
            draining: false,
            server_changed: false,
            build_note: None,
            home_note: None,
            quota: None,
        }
    }

    /// The model the server reported, for a caller that only says what it reached.
    pub fn model(&self) -> &str {
        &self.model
    }

    /// Whether a `connect_server` moved this client, once per move.
    pub fn take_server_change(&mut self) -> bool {
        std::mem::take(&mut self.server_changed)
    }

    /// The first id this client will use.
    ///
    /// # Invariant
    ///
    /// Hard for separate processes to collide. Command results are connection-
    /// local, but distinct ids also keep logs and pending follows unambiguous.
    fn seed_command_id() -> u64 {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(0);
        (nanos ^ ((std::process::id() as u64) << 32)) | 1
    }

    fn next_id(&mut self) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        id
    }

    /// Feeds one datagram from the link into the client, decoding it here.
    pub fn handle_bytes(&mut self, bytes: &[u8], sink: &mut dyn Sink) -> anyhow::Result<()> {
        let msg = decode_server(bytes)?;
        self.handle(msg, sink);
        Ok(())
    }

    /// The completion source for the input line, shared rather than copied.
    pub fn completer(&self) -> crate::SharedCompleter {
        Arc::new(self.catalog.clone())
    }

    /// The commands the server published, as published.
    pub fn commands(&self) -> Vec<protocol::CommandSpec> {
        self.catalog.commands()
    }

    fn handle(&mut self, msg: ServerMessage, sink: &mut dyn Sink) {
        match msg {
            ServerMessage::Hello {
                server,
                commands,
                sessions,
                models,
                ..
            } => {
                self.model = server.model.clone();
                // A redial is a resync, so a session deleted while this client was
                // away leaves the list here too.
                self.set_sessions(sessions, sink);
                self.catalog.set_commands(commands);
                self.catalog.set_models(models);
                self.catalog.set_default_model(&server.model);
                // Where a session with no directory of its own starts, and the
                // directory a relative value is read from until one says.
                self.catalog
                    .set_roots(self.sticky.home(), Path::new(&server.cwd));
                self.sync_sticky();
                // A greeting *is* the recovery: it carries the catalog and
                // sessions, so a redial comes up like a fresh client.
                self.connected = true;
                self.attempts = 0;
                self.draining = server.draining;
                // The announcement is the facts: the model and the session count.
                // A warning belongs beside them.
                self.build_note = (!server.build.is_empty() && server.build != protocol::BUILD)
                    .then(|| format!("build {}", server.build));
                let mine = self.sticky.home();
                self.home_note = (!server.home.is_empty() && !same_home(&server.home, mine))
                    .then(|| format!("home {}", server.home));
                self.refresh(sink);
            }
            ServerMessage::Denied { reason } => {
                sink.note(LineKind::Error, &format!("connection refused: {reason}"));
            }
            ServerMessage::Event(event) => self.handle_event(event, sink),
            // A kind this build does not know: the tag is what a reader needs;
            // the payload is the sending side's business.
            ServerMessage::Unknown { event, .. } => {
                let kind = event
                    .get("t")
                    .and_then(|t| t.as_str())
                    .unwrap_or("?")
                    .to_string();
                let session = event.get("session").and_then(|s| s.as_str());
                sink.note(
                    LineKind::System,
                    &format!(
                        "[{kind}] an event this build does not know (session: {})",
                        session.unwrap_or("-")
                    ),
                );
            }
        }
    }

    /// Records a `list_paths` answer, so the popup offers it on the next ask.
    fn fill_listing(&mut self, data: &serde_json::Value) {
        let field = |name: &str| data.get(name).and_then(|v| v.as_str()).unwrap_or_default();
        let Some(entries) = data.get("entries").and_then(|v| v.as_array()) else {
            return;
        };
        let rows: Vec<(String, bool)> = entries
            .iter()
            .filter_map(|entry| {
                Some((
                    entry.get("name")?.as_str()?.to_string(),
                    entry.get("dir")?.as_bool()?,
                ))
            })
            .collect();
        self.catalog
            .fill_paths(field("remote"), field("path"), rows);
    }

    /// One event from the server, given to the front-end whatever window it names.
    ///
    /// # Invariant
    ///
    /// Nothing here is dropped for belonging to a hidden session:
    /// `set_render_session` changes what a frame draws, never what the client is
    /// told. A front-end that records what it is handed -- the TUI's journal --
    /// therefore keeps a hidden session's rows, and showing it again draws them.
    /// See `docs/notes/cli-lib.md`.
    fn handle_event(&mut self, event: Event, sink: &mut dyn Sink) {
        match event {
            Event::Cmd { id, name, args } => {
                // A question the completion asked is not a line of the operator's,
                // so it is not shown, and its answer is not either.
                if self.quiet.contains_key(&id) {
                    return;
                }
                let session = args.get("session").map(String::as_str);
                let window = self.command_line(session);
                // The result carries only the id, so say now where its answer goes.
                self.results.insert(id, window.clone());
                // No `$`: SPEC's `cmd` item carries a symbol, and the mark
                // that says "this is a command" is the front-end's to draw.
                let line = format!(
                    "{name}{}",
                    render_args(&args, self.catalog.command(&name).as_ref())
                );
                // SPEC's row is `last cmd | cmd_result`: a command of no
                // session replaces the one before.
                if session.is_none() {
                    sink.clear(&window);
                }
                sink.line(&window, LineKind::Command, &line);
            }
            Event::CmdResult {
                id,
                message,
                ok,
                data,
                ..
            } => {
                if let Some(remote) = self.quiet.remove(&id) {
                    match (ok, data.as_ref()) {
                        (true, Some(named)) => self.fill_listing(named),
                        // Asked again next time: a refusal is not an answer.
                        _ => self.catalog.fail_paths(&remote),
                    }
                    return;
                }
                self.settle_follow(id, ok, sink);
                // The rows a read answered with arrived as events before this;
                // what is left to say is the sentence about them.
                let window = self.results.remove(&id).unwrap_or_else(|| {
                    self.command_line(
                        data.as_ref()
                            .and_then(|d| d.get("session"))
                            .and_then(|s| s.as_str()),
                    )
                });
                // The session this answer was addressed to may be the one the
                // command just deleted, so its window is already gone.
                let window = self.answer_window(window);
                if !message.is_empty() {
                    let kind = if ok {
                        LineKind::Result
                    } else {
                        LineKind::Error
                    };
                    // The answer replaces the `cmd` line above it: one row is
                    // the last of the two, not both stacked.
                    if window == self.command_window() {
                        sink.clear(&window);
                    }
                    sink.line(&window, kind, &message);
                }
            }
            Event::Response(e) => self.handle_response(e, sink),
            Event::ToolCall(e) => self.handle_tool_call(e, sink),
            Event::ToolAnswer(e) => self.handle_tool_answer(e, sink),
            Event::Sessions { sessions } => {
                self.set_sessions(sessions, sink);
                self.refresh(sink);
            }
            // An edited roster: silent, because nobody asked and nothing failed.
            Event::Models { models } => self.catalog.set_models(models),
            Event::Status {
                session,
                status,
                detail,
            } => {
                if let Some(s) = self.sessions.iter_mut().find(|s| s.name == session) {
                    s.status = status;
                }
                let label = match status {
                    SessionStatus::Idle => "idle",
                    SessionStatus::Running => "running",
                    SessionStatus::Error => "error",
                };
                let suffix = detail.map(|d| format!(" ({d})")).unwrap_or_default();
                let window = session.clone();
                let prefix = self.tag(&session, "");
                sink.line(
                    &window,
                    LineKind::System,
                    &format!("{prefix}{label}{suffix}"),
                );
                // A turn that ended is a stop, and a stop is what the window
                // outside this one is told about.
                if status != SessionStatus::Running {
                    sink.stopped(&session, status == SessionStatus::Error);
                }
                self.refresh(sink);
            }
            // SPEC's three numbers for a full pane's title: a delta, so a view
            // that is scrolled up stays put as they change.
            Event::Usage(e) => {
                if let Some(s) = self.sessions.iter_mut().find(|s| s.name == e.session) {
                    s.usage = e.usage;
                }
                self.refresh(sink);
            }
            // SPEC's `active_quota` row: one line under the command line, told
            // again at every step's end.
            Event::Quota(quota) => {
                let row = quota_row(&quota);
                if self.quota.as_ref() != Some(&row) {
                    self.quota = Some(row);
                    self.refresh(sink);
                }
            }
            // A log row is data, not conversation: the window draws what was said,
            // and `events` is how a reader asks for the rest.
            Event::Record { .. } => {}
            Event::Log { level, message } => {
                let kind = match level {
                    LogLevel::Debug | LogLevel::Info => LineKind::System,
                    LogLevel::Warn | LogLevel::Error => LineKind::Error,
                };
                sink.note(kind, &message);
            }
        }
    }

    fn handle_response(&mut self, e: ResponseEvent, sink: &mut dyn Sink) {
        // One part is one block. A call streams under its result's id, so the
        // structured event fills the block it opened.
        let key = match (&e.call_id, e.part) {
            (Some(call_id), ResponsePart::ToolCall) => format!("c:{call_id}"),
            _ => format!("r:{}:{}:{:?}", e.session, e.turn, e.part),
        };
        let (kind, label) = match e.part {
            // No label: the kind's colour (dim italic) already says this is the
            // model thinking, and the label was the word twice.
            ResponsePart::Reasoning => (LineKind::Reasoning, ""),
            // No label: the colour says a tool ran. Only a provider's own call
            // streams now; our tools' calls arrive whole.
            ResponsePart::ToolCall => (LineKind::ToolCall, ""),
            // Kept: it shares the tool call's colour, so the word is the only
            // thing saying who ran it.
            ResponsePart::Activity => (LineKind::ToolCall, "provider: "),
            ResponsePart::Output => (LineKind::Assistant, ""),
        };
        // The tag says which part of the answer this is; the window says who speaks.
        let prefix = self.tag(&e.session, label);
        let window = e.session.clone();
        let done = e.done && e.part != ResponsePart::ToolCall;
        sink.stream(&window, &key, kind, &prefix, &e.text, done);
    }

    /// A tool the model asked for: one block per call, so two calls in one turn
    /// stay two items.
    ///
    /// # Invariant
    ///
    /// The call id pairs this block with its result; their bodies stay separate.
    fn handle_tool_call(&mut self, e: protocol::ToolCallEvent, sink: &mut dyn Sink) {
        let key = format!("c:{}", e.call_id);
        let streamed = !self.calls.insert(e.call_id.clone());
        sink.tool_call(&e.session, &key, &e.head, streamed);
    }

    /// A call's output, as one item: SPEC's `tool_result{ok, time (spent)}`.
    ///
    /// # Invariant
    ///
    /// Call and result are separate blocks, paired by call id. Only the final
    /// verdict supplies ✓/✕: an stderr chunk is output, not a failed call. The
    /// label is the verdict alone, because the call's summary is the row above
    /// and a result that repeated it would say the same tool twice.
    fn handle_tool_answer(&self, e: ToolAnswerEvent, sink: &mut dyn Sink) {
        let key = format!("t:{}", e.call_id);
        let (kind, done) = match &e.head {
            Some(head) => (kind_of(head.kind), true),
            // Output streams do not decide success or failure.
            None => (LineKind::ToolOutput, false),
        };
        let outcome = match &e.head {
            Some(head) => head_label(head),
            // Nothing is known yet, so the block opens bare and output is its own.
            None => String::new(),
        };
        // A verdict changes the existing result's label, never its identity.
        sink.stream(&e.session, &key, kind, &outcome, &e.text, done);
        if done {
            sink.relabel(&e.session, &key, &outcome, kind);
        }
    }

    /// Replaces the session list, telling the front-end what left it.
    ///
    /// # Invariant
    ///
    /// A name leaving the list is a deletion: both announcements carry every
    /// session, and nothing else takes one out. Its window is emptied here,
    /// because the server hands the name out again -- a new session must not open
    /// on the deleted one's lines. See `docs/notes/cli-lib.md`.
    fn set_sessions(&mut self, sessions: Vec<SessionInfo>, sink: &mut dyn Sink) {
        for old in &self.sessions {
            if !sessions.iter().any(|new| new.name == old.name) {
                sink.forget(&old.name);
            }
        }
        // As published: a value completed for a session belongs to that
        // session's directory and machine.
        self.catalog.set_sessions(sessions.clone());
        self.sessions = sessions;
    }

    /// Keeps the completer's state-backed sources in step: the sticky names, and
    /// the session a bare message goes to.
    pub fn sync_sticky(&self) {
        self.catalog.set_sticky_names(&self.sticky.names());
        self.catalog.set_message_session(self.sticky.get("session"));
    }

    /// The status line: server facts only, never a "current session".
    /// See `docs/notes/cli-lib.md`.
    pub fn refresh(&self, sink: &mut dyn Sink) {
        // Told on every refresh, so a front-end attached late still learns it.
        sink.block_size(self.block_size.0);
        sink.box_edge(self.box_edge.0);
        sink.layout(&self.layout());
        sink.sticky_session(self.sticky.get("session"));
        let mut facts = if self.connected && self.draining {
            // Connected and stopping: the one thing the reader cannot work out
            // from silence, said outright.
            "stopping — the server keeps answering until its turns finish".to_string()
        } else if self.connected {
            format!("{} session(s)", self.sessions.len())
        } else if self.attempts == 0 {
            "offline — connecting…".to_string()
        } else {
            format!("offline — reconnecting (attempt {})", self.attempts)
        };
        for note in [&self.build_note, &self.home_note].into_iter().flatten() {
            facts.push_str(" | ");
            facts.push_str(note);
        }
        sink.status(&facts);
    }

    /// Records that there is no link, and how many dials have failed since.
    pub fn set_offline(&mut self, attempts: u32, sink: &mut dyn Sink) {
        self.connected = false;
        self.results.clear();
        self.attempts = attempts;
        self.refresh(sink);
    }

    /// Runs one command from argv already split into tokens; no grammar here.
    pub fn command(&mut self, tokens: Vec<String>, sink: &mut dyn Sink) -> Submission {
        self.submit_tokens(tokens, sink, Echo::Command)
    }

    /// The listings the completion asked for and has no answer for yet.
    pub fn take_wanted_paths(&self) -> Vec<(String, String)> {
        self.catalog.take_wanted()
    }

    /// How many listings have landed; a front-end repaints when this changes.
    pub fn paths_version(&self) -> u64 {
        self.catalog.paths_version()
    }

    /// Asks the server what one directory on a remote holds, for `--cwd`; `None`
    /// when there is nothing to ask.
    ///
    /// # Invariant
    ///
    /// Never echoes the command: it is the front-end's own question, and the
    /// operator's last line is not replaced by it.
    pub fn ask_paths(&mut self, remote: &str, directory: &str) -> Option<ClientMessage> {
        if !self.connected || self.catalog.command("list_paths").is_none() {
            return None;
        }
        let id = self.next_id();
        let mut args = protocol::Args::new();
        args.insert("remote".to_string(), remote.to_string());
        args.insert("path".to_string(), directory.to_string());
        self.quiet.insert(id, remote.to_string());
        Some(ClientMessage::Command {
            id,
            name: "list_paths".to_string(),
            args,
        })
    }

    /// Sends one line of prose: SPEC's UI expands it to `send_message`, the
    /// session being the sticky one.
    ///
    /// # Invariant
    ///
    /// The expansion names only the message; the session argument fills from
    /// the sticky `session` entry like any omitted argument, and the server
    /// refuses a line that names neither -- the refusal says so. No sticky
    /// session is still said here, not sent as a message to nobody.
    pub fn message(&mut self, text: &str, sink: &mut dyn Sink) -> Submission {
        if self.sticky.get("session").is_none() {
            self.row(
                sink,
                LineKind::Error,
                "no session to send to: set one with `/set_sticky session <name>`, or start the line with `/` and name the command",
            );
            return Submission::Rejected;
        }
        self.submit_tokens(
            vec![
                "send_message".to_string(),
                "--message".to_string(),
                text.to_string(),
            ],
            sink,
            Echo::Message(text),
        )
    }

    /// The one path both entries take. `echo` only changes what is shown.
    fn submit_tokens(
        &mut self,
        tokens: Vec<String>,
        sink: &mut dyn Sink,
        echo: Echo<'_>,
    ) -> Submission {
        let Some(head) = tokens.first().cloned() else {
            return Submission::Rejected;
        };

        // Offline, the applied catalog is stale: say so, and leave client
        // commands alone.
        if !self.connected && !self.catalog.is_client_command(&head) {
            self.row(
                sink,
                LineKind::Error,
                &format!("{head}: not connected to the server, nothing was sent"),
            );
            return Submission::Offline;
        }

        let Some(spec) = self.catalog.command(&head) else {
            // `/name` and a Git Bash-rewritten path get the line that was
            // meant. See `docs/notes/cli-lib.md`.
            if head.contains('/') || head.contains('\\') {
                let tail = match head.rfind(['/', '\\']) {
                    Some(at) => &head[at + 1..],
                    None => head.as_str(),
                };
                let name = tail.split_whitespace().next().unwrap_or_default();
                // Whatever followed the name, in either shape.
                let mut rest = tail[name.len()..].trim().to_string();
                if tokens.len() > 1 {
                    if !rest.is_empty() {
                        rest.push(' ');
                    }
                    rest.push_str(&tokens[1..].join(" "));
                }
                if self.catalog.command(name).is_some() {
                    self.row(
                        sink,
                        LineKind::Error,
                        &format!(
                            "`{head}` is not a command name: it ends in `{name}`, and a `/` \
                             line is the interactive input line's syntax (on Windows, Git Bash \
                             also rewrites a leading `/` into a path before this program runs). \
                             From a command line, say the command outright: `gongga {name}{rest}`",
                            rest = if rest.is_empty() {
                                String::new()
                            } else {
                                format!(" {rest}")
                            }
                        ),
                    );
                    return Submission::Rejected;
                }
            }
            let known: Vec<String> = self
                .catalog
                .commands()
                .iter()
                .map(|c| c.name.clone())
                .collect();
            // No greeting yet means no list: the server owns it.
            let why = if known.is_empty() {
                "the command list comes from the server and it has not sent one yet: \
                 connect, then ask again"
                    .to_string()
            } else {
                format!("this server publishes: {}", known.join(", "))
            };
            self.row(
                sink,
                LineKind::Error,
                &format!("unknown command {head:?}; {why}"),
            );
            return Submission::Rejected;
        };

        // Validation by the published spec; `remote` picks the path. A
        // token it does not name is a typo, said before sending.
        let mut args = match parse_args(&tokens[1..], Some(&spec)) {
            Ok(a) => a,
            Err(e) => {
                self.row(sink, LineKind::Error, &e.to_string());
                return Submission::Rejected;
            }
        };
        // Typed arguments win: a sticky fills only a slot still empty, the
        // same rule the flag spelling had.
        self.sticky.fill(Some(&spec), &mut args);
        let missing: Vec<&str> = spec
            .args
            .iter()
            .filter(|a| a.required && !args.contains_key(&a.name))
            .map(|a| a.name.as_str())
            .collect();
        if !missing.is_empty() {
            // The window the error belongs to is the session the command names
            // -- wherever its spec keeps the kind-`session` argument.
            let session = spec
                .args
                .iter()
                .find(|a| a.kind.as_deref() == Some("session"))
                .and_then(|a| args.get(&a.name))
                .map(String::as_str);
            self.show(sink, &echo, &tokens, session);
            self.row(
                sink,
                LineKind::Error,
                &format!(
                    "{head} is missing required argument(s): {}; it takes {}",
                    missing.join(", "),
                    commands::signature(Some(&spec)),
                ),
            );
            return Submission::Rejected;
        }

        // One echo per command: this one, where it answers. A server command is
        // drawn from its `cmd` event.
        if spec.target == protocol::CommandTarget::Client {
            self.show(sink, &echo, &tokens, None);
            self.run_client_command(&head, &args, sink);
            return Submission::Local;
        }

        let id = self.next_id();
        // SPEC: the sticky follows what the server accepted, never a submission.
        // What an accepted set_session writes back is decided here, from the
        // bound arguments: every argument with a kind that the command carries,
        // keyed by kind -- an argument the command did not name is not written,
        // so a partial set_session never erases the entries it left alone.
        if head == "set_session" {
            let pairs: Vec<(String, String)> = spec
                .args
                .iter()
                .filter(|a| a.kind.is_some() && args.contains_key(&a.name))
                .filter_map(|a| {
                    a.kind
                        .clone()
                        .and_then(|kind| args.get(&a.name).map(|value| (kind, value.clone())))
                })
                .collect();
            if !pairs.is_empty() {
                self.follows.push((id, pairs));
            }
        }
        Submission::Send(ClientMessage::Command {
            id,
            name: head,
            args,
        })
    }

    /// `connect_server`: the address a later command dials, written down now.
    ///
    /// # Invariant
    ///
    /// The move is saved before the front-end is told, so a window that
    /// redials at once reads the new address rather than the old one.
    fn remember_server(&mut self, args: &protocol::Args, sink: &mut dyn Sink) {
        let port = args.get("port").map(String::as_str).unwrap_or_default();
        let ip = args.get("ip").map(String::as_str);
        let addr = match server_address(ip, port) {
            Ok(addr) => addr,
            Err(e) => {
                self.row(sink, LineKind::Error, &format!("{e}"));
                return;
            }
        };
        match commands::Remembered::save(self.sticky.home(), addr) {
            Ok(()) => {
                self.row(
                    sink,
                    LineKind::Result,
                    &format!("server is {addr}; reconnecting"),
                );
                self.server_changed = true;
            }
            Err(e) => self.row(sink, LineKind::Error, &format!("could not save: {e}")),
        }
    }

    /// Applies a `set_session` the server accepted; a refusal writes nothing.
    ///
    /// # Invariant
    ///
    /// It runs on the acknowledgement, not on the submission: a refused
    /// `set_session` must not retarget the next message.
    fn settle_follow(&mut self, id: u64, ok: bool, sink: &mut dyn Sink) {
        let Some(at) = self.follows.iter().position(|(i, _)| *i == id) else {
            return;
        };
        let (_, pairs) = self.follows.remove(at);
        if !ok {
            return;
        }
        // Each pair is a mode the next command inherits: the session, and
        // every bundle member the accepted command carried.
        let saved = pairs
            .iter()
            .map(|(kind, value)| self.sticky.set(kind, value))
            .find(|saved| saved.is_err());
        if let Some(Err(e)) = saved {
            self.row(sink, LineKind::Error, &format!("could not save: {e}"));
        }
        self.after_sticky_change(sink);
    }

    /// Applies one operation: the API a window's gestures use.
    ///
    /// # Invariant
    ///
    /// An operation is not a command line: the window asks for "make this
    /// session sticky", and the client answers it the same way the command
    /// does -- one behavior per gesture, whichever way it was asked for.
    pub fn apply(&mut self, operation: protocol::Operation, sink: &mut dyn Sink) -> Submission {
        match operation {
            protocol::Operation::StickySession { session } => {
                self.stick("session", &session, sink);
                Submission::Local
            }
            protocol::Operation::RenderSession { session, show } => {
                self.set_shown(&session, show, sink);
                Submission::Local
            }
            protocol::Operation::DeleteSession { .. } => {
                let id = self.next_id();
                Submission::Send(ClientMessage::Operation { id, operation })
            }
        }
    }

    /// Makes one argument sticky, and says what it is now.
    fn stick(&mut self, name: &str, value: &str, sink: &mut dyn Sink) {
        match self.sticky.set(name, value) {
            Ok(()) => {
                self.row(sink, LineKind::Result, &format!("sticky {name} = {value}"));
                self.after_sticky_change(sink);
            }
            Err(e) => self.row(sink, LineKind::Error, &format!("could not save: {e}")),
        }
    }

    /// Shows or hides one session's window.
    fn set_shown(&mut self, session: &str, show: bool, sink: &mut dyn Sink) {
        if let Err(e) = self.shown.set(session, show) {
            self.row(sink, LineKind::Error, &format!("could not save: {e}"));
            return;
        }
        self.row(
            sink,
            LineKind::Result,
            &format!("render {session} = {show}"),
        );
        self.follow_shown(session, show, sink);
        self.after_sticky_change(sink);
    }

    /// Runs one client command, whose args the spec already validated.
    fn run_client_command(&mut self, name: &str, args: &protocol::Args, sink: &mut dyn Sink) {
        let arg_name = args.get("type").cloned().unwrap_or_default();
        match name {
            "connect_server" => self.remember_server(args, sink),
            // The process is the server: a window cannot draw it, so say
            // where to type it.
            "serve" => self.row(
                sink,
                LineKind::Error,
                &format!(
                    "serve takes over the process; run it in a shell: `{} serve <port>`",
                    typed_name()
                ),
            ),
            "set_render" => self.run_render(args, sink),
            "set_render_session" => self.run_render_session(args, sink),
            "set_sticky" => {
                let Some(value) = args.get("value").cloned() else {
                    self.row(
                        sink,
                        LineKind::Error,
                        &format!(
                            "set_sticky is missing required argument(s): value; it takes {}",
                            commands::signature(self.catalog.command("set_sticky").as_ref()),
                        ),
                    );
                    return;
                };
                // A sticky entry no argument declares never takes effect, so it
                // is refused while the caller is still looking.
                if self.catalog.fills_sticky(&arg_name) == Some(false) {
                    self.row(
                        sink,
                        LineKind::Error,
                        &format!(
                            "{arg_name:?} is not a type any published argument declares, so no \
                             sticky entry can use it; the types are: {}",
                            self.catalog.sticky_names().join(", ")
                        ),
                    );
                    return;
                }
                self.stick(&arg_name, &value, sink);
            }
            "del_sticky" => match self.sticky.del(&arg_name) {
                Ok(true) => {
                    self.row(
                        sink,
                        LineKind::Result,
                        &format!("sticky {arg_name} cleared"),
                    );
                    self.after_sticky_change(sink);
                }
                Ok(false) => self.row(
                    sink,
                    LineKind::Error,
                    &format!("no sticky argument named {arg_name:?}"),
                ),
                Err(e) => self.row(sink, LineKind::Error, &format!("could not save: {e}")),
            },
            // Published as a client command this build cannot run: say so.
            other => self.row(
                sink,
                LineKind::Error,
                &format!(
                    "the server publishes {other:?} as a client command, but this client \
                     has no handler for it: the two are from different builds"
                ),
            ),
        }
    }

    /// Every answer names its own subject: the ticker keeps only the last row.
    ///
    /// Runs `set_render`: how tall a block may get.
    ///
    /// # Invariant
    ///
    /// A value that does not parse saves nothing, and no argument is a read of
    /// the setting rather than a reset of it.
    fn run_render(&mut self, args: &protocol::Args, sink: &mut dyn Sink) {
        let size = args.get("block_size");
        let edge = args.get("box_edge");
        if size.is_none() && edge.is_none() {
            let report = self.render_report();
            self.row(sink, LineKind::Result, &report);
            return;
        }
        // Both parsed before either is saved: half of a setting that does not parse
        // is a value nobody asked for.
        let size = match size.map(|raw| BlockSize::parse(raw)).transpose() {
            Ok(size) => size,
            Err(e) => return self.row(sink, LineKind::Error, &e),
        };
        let edge = match edge.map(|raw| BoxEdge::parse(raw)).transpose() {
            Ok(edge) => edge,
            Err(e) => return self.row(sink, LineKind::Error, &e),
        };
        let saved = size
            .map(|size| size.save(self.sticky.home()))
            .into_iter()
            .chain(edge.map(|edge| edge.save(self.sticky.home())))
            .find(|saved| saved.is_err());
        if let Some(Err(e)) = saved {
            return self.row(sink, LineKind::Error, &format!("could not save: {e}"));
        }
        if let Some(size) = size {
            self.block_size = size;
        }
        if let Some(edge) = edge {
            self.box_edge = edge;
        }
        let report = self.render_report();
        self.row(sink, LineKind::Result, &report);
        self.after_sticky_change(sink);
    }

    /// What `set_render` is set to, in one row: both numbers, in the row's own words.
    fn render_report(&self) -> String {
        format!(
            "render block_size = {}, box_edge = {}",
            self.block_size.0, self.box_edge.0
        )
    }

    /// Runs `set_render_session`: whether one session is shown beside the list.
    ///
    /// # Invariant
    ///
    /// It changes a view and never the state: the blocks a session already said are
    /// still held, so `true` after `false` brings them back. SPEC's
    /// click is this command, so the sticky session follows the window: shown makes
    /// it sticky, hidden and sticky clears the sticky. See `docs/notes/cli-lib.md`.
    fn run_render_session(&mut self, args: &protocol::Args, sink: &mut dyn Sink) {
        let name = args.get("session").cloned().unwrap_or_default();
        let Some(raw) = args.get("show") else {
            let show = self.shown.is_shown(&name);
            self.row(sink, LineKind::Result, &format!("render {name} = {show}"));
            return;
        };
        let show = match commands::parse_show(raw) {
            Ok(show) => show,
            Err(e) => {
                self.row(sink, LineKind::Error, &e);
                return;
            }
        };
        self.set_shown(&name, show, sink);
    }

    /// Points the sticky session at the window just shown, or clears it.
    ///
    /// # Invariant
    ///
    /// Only the session that was sticky is cleared: hiding another session is no
    /// statement about which window the operator is reading.
    fn follow_shown(&mut self, name: &str, show: bool, sink: &mut dyn Sink) {
        let saved = if show {
            self.sticky.set("session", name)
        } else if self.sticky.get("session") == Some(name) {
            self.sticky.del("session").map(|_| ())
        } else {
            Ok(())
        };
        if let Err(e) = saved {
            self.row(sink, LineKind::Error, &format!("could not save: {e}"));
        }
    }

    fn after_sticky_change(&self, sink: &mut dyn Sink) {
        self.sync_sticky();
        self.refresh(sink);
    }
}

/// SPEC's `full` pane title: the measurements, then what the session runs on.
///
/// # Invariant
///
/// Each figure carries SPEC's unit, and one nobody measured is left out rather than
/// shown as zero: `ctx 0` reads as an empty context, not a request that never
/// happened. See `docs/notes/cli-lib.md`.
fn title_note(session: &protocol::SessionInfo) -> String {
    let usage = &session.usage;
    let mut parts: Vec<String> = Vec::new();
    if usage.context_tokens > 0 {
        parts.push(format!("ctx {}", usage.context_tokens));
        // SPEC says the share, so the client does the division, not the reader.
        if let Some(cached) = usage.cached_tokens {
            parts.push(format!(
                "cached {}%",
                cached.saturating_mul(100) / usage.context_tokens
            ));
        }
    }
    if let Some(tps) = usage.tps {
        parts.push(format!("tps {tps:.1}"));
    }
    if let Some(ms) = usage.ttft_ms {
        parts.push(match ms {
            0..1000 => format!("ttft {ms}ms"),
            _ => format!("ttft {:.1}s", ms as f64 / 1000.0),
        });
    }
    if !session.cwd.is_empty() {
        parts.push(format!("cwd {}", session.cwd));
    }
    if let Some(model) = &session.model {
        parts.push(format!("model {model}"));
    }
    if let Some(level) = &session.thinking_effort {
        parts.push(format!("thinking {level}"));
    }
    if let Some(percent) = session.context_limit {
        parts.push(match percent {
            0 => "ctx_limit off".to_string(),
            percent => format!("ctx_limit {percent}%"),
        });
    }
    parts.join(" · ")
}

/// SPEC's `active_quota` row: the provider, then the figures it reported.
///
/// # Invariant
///
/// The label is the name SPEC gives each figure -- `month`, `week`, `5h`,
/// `credit`, `money` -- and one the provider did not report is left out rather
/// than shown as zero. See `docs/notes/cli-lib.md`.
fn quota_row(quota: &protocol::Quota) -> QuotaRow {
    let mut parts: Vec<String> = Vec::new();
    for (label, share) in [
        ("month", quota.month_percent),
        ("week", quota.week_percent),
        ("5h", quota.five_hour_percent),
    ] {
        if let Some(share) = share {
            parts.push(format!("{label} {share:.0}%"));
        }
    }
    if let Some(credit) = quota.credit {
        parts.push(format!("credit {}", amount(credit)));
    }
    if let Some(money) = quota.money {
        parts.push(format!("money {}{}", quota.currency, amount(money)));
    }
    if let Some(note) = &quota.note {
        parts.push(note.clone());
    }
    let text = if parts.is_empty() {
        quota.provider.clone()
    } else {
        format!("{} · {}", quota.provider, parts.join(" · "))
    };
    // A row naming only its provider and a complaint is a failure to read;
    // figures make it a fact.
    let kind = match (&quota.note, parts.len()) {
        (Some(_), 1) => LineKind::Error,
        _ => LineKind::System,
    };
    QuotaRow { text, kind }
}

/// A money or credit figure: two decimals when the cents matter, none when the
/// provider reports a whole count.
fn amount(value: f64) -> String {
    if (value.fract()).abs() < f64::EPSILON {
        format!("{value:.0}")
    } else {
        format!("{value:.2}")
    }
}

// Front-ends

/// One open link: the two halves of the channel, kept together because a link
/// is either wholly there or wholly gone.
pub struct Link {
    sink: protocol::ChannelSink,
    stream: protocol::ChannelStream,
}

impl Link {
    /// Hands one message to the channel task.
    pub async fn send(&self, msg: ClientMessage) -> anyhow::Result<()> {
        let bytes = encode_client(&msg)?;
        self.sink.send(bytes).await.map_err(|e| anyhow!("{e}"))
    }

    /// Next datagram, or `None` when the link ended.
    pub async fn recv(&mut self) -> Option<Vec<u8>> {
        self.stream.recv().await
    }

    /// One that has already arrived, without waiting.
    pub fn try_recv(&mut self) -> Option<Vec<u8>> {
        self.stream.try_recv()
    }

    /// Best-effort goodbye, so the server can drop this connection now.
    pub async fn goodbye(&self) {
        let _ = self.send(ClientMessage::Bye).await;
    }

    /// Stops the server as `cancel` does, and answers whether it heard.
    ///
    /// # Invariant
    ///
    /// SPEC's stop, not a kill: a kill loses the work `cancel` stores for the next
    /// start. The answer is what makes it a stop and not a wish -- `send` only
    /// queues, and a window asking on its way out is one statement away from
    /// dropping the runtime that would write it. See `docs/notes/cli-lib.md`.
    pub async fn cancel(&mut self) -> bool {
        let asked = self
            .send(ClientMessage::Command {
                id: CANCEL_ID,
                name: "cancel".to_string(),
                args: Default::default(),
            })
            .await;
        if asked.is_err() {
            return false;
        }
        while let Some(bytes) = self.recv().await {
            if let Ok(ServerMessage::Event(Event::CmdResult { id, .. })) = decode_server(&bytes)
                && id == CANCEL_ID
            {
                return true;
            }
        }
        false
    }
}

/// The id a way-out's `cancel` carries: nothing else is outstanding on a link
/// that is closing.
const CANCEL_ID: u64 = u64::MAX;

/// Runs one command line, with the runtime each command needs.
///
/// # Invariant
///
/// `serve` is decided before a runtime exists: it leaves nothing of the client
/// behind, and the server builds the runtime it wants to be stopped in.
pub fn main(args: Args) -> anyhow::Result<()> {
    if let Some(Sub::Cmd(tokens)) = &args.command {
        match tokens.first().map(String::as_str) {
            Some("serve") => return crate::server::serve(&args, &tokens[1..]),
            // Nothing here needs a runtime or a link: an entry is a file on this disk.
            Some("skill") => {
                return crate::skill_command::dispatch(&home_of(&args), &tokens[1..]);
            }
            _ => {}
        }
    }
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    runtime.block_on(async {
        match &args.command {
            None => list_commands(&args).await,
            Some(Sub::Cmd(command)) => run(&args, command).await,
        }
    })
}

/// An address a client is asked to dial: `--ip` (localhost by default), `--port`.
///
/// # Invariant
///
/// The default is `127.0.0.1`, not the name `localhost`: that is what `serve`
/// binds, and this platform resolves `localhost` to `::1` first -- the two ends
/// of one command line must agree without asking DNS.
pub fn server_address(ip: Option<&str>, port: &str) -> anyhow::Result<SocketAddr> {
    let ip = ip.unwrap_or("127.0.0.1");
    let host = format!("{ip}:{port}");
    let resolved: Vec<SocketAddr> = host
        .to_socket_addrs()
        .with_context(|| format!("resolving {host:?}"))?
        .collect();
    resolved
        .iter()
        .copied()
        .find(SocketAddr::is_ipv4)
        .or_else(|| resolved.first().copied())
        .ok_or_else(|| anyhow!("{host:?} is not an address"))
}

/// `connect_server`: remember where the next command goes, then prove it answers.
async fn connect_elsewhere(args: &Args, command: &[String]) -> anyhow::Result<()> {
    let spec = protocol::client_commands()
        .into_iter()
        .find(|c| c.name == "connect_server");
    let parsed = parse_args(&command[1..], spec.as_ref())?;
    let port = parsed.get("port").map(String::as_str).unwrap_or_default();
    let addr = server_address(parsed.get("ip").map(String::as_str), port)?;
    // Saved before dialing: a refusal then leaves the client pointed where
    // the operator asked.
    commands::Remembered::save(&home_dir(args.conn.home.as_deref()), addr)?;
    let key = resolve_key(&args.conn).ok_or_else(|| {
        anyhow!(
            "{addr} is remembered as this client's server, but there is no key to dial it              with: pass --key, set GONGGA_KEY, or start the server once to generate one"
        )
    })?;
    let (link, greeting) = dial(Endpoint { addr, key })
        .await
        .map_err(|failure| failure.into_error())?;
    let mut client = build_client(args);
    greeting.apply(&mut client, &mut NullSink::default());
    println!(
        "{}",
        serde_json::json!({
            "server": addr.to_string(),
            "connected": true,
            "model": client.model(),
        })
    );
    link.goodbye().await;
    Ok(())
}

/// Headless: one command in, its JSON events out, then exit.
///
/// See `docs/notes/cli-lib.md`.
pub async fn run(args: &Args, command: &[String]) -> anyhow::Result<()> {
    // `connect_server` says where a link would go; it cannot need one first.
    if command.first().map(String::as_str) == Some("connect_server") {
        return connect_elsewhere(args, command).await;
    }
    let (mut client, mut link) = connect(args).await?;
    let result = run_once(&mut client, &mut link, command).await;
    // A best-effort goodbye lets the server tear the connection down now
    // instead of waiting out the idle timeout.
    link.goodbye().await;
    result
}

async fn run_once(client: &mut Client, link: &mut Link, command: &[String]) -> anyhow::Result<()> {
    let mut printer = NullSink::default();
    let msg = match client.command(command.to_vec(), &mut printer) {
        // Nothing to send, but a refusal it reported must still fail.
        Submission::Local => {
            return match printer.take_error() {
                Some(why) => Err(anyhow!(why)),
                None => Ok(()),
            };
        }
        // Rejected before the server saw it, so there is no event; the reason
        // still has to come out.
        Submission::Rejected => {
            let why = printer
                .take_error()
                .unwrap_or_else(|| "not a command".to_string());
            anyhow::bail!("{why}");
        }
        // Cannot happen here, but a silent `todo!()` would be worse than
        // saying so.
        Submission::Offline => anyhow::bail!("not connected to the server"),
        // Nothing to send: what it waits for is already arriving on this link.
        Submission::Send(msg) => msg,
    };
    let id = match &msg {
        ClientMessage::Command { id, .. } => *id,
        other => anyhow::bail!("{command:?} produced {other:?}, expected a command"),
    };
    // This command's own session, if it names one: another session's stream is
    // not this process's output.
    let watched = match &msg {
        ClientMessage::Command { args, .. } => args.get("session").cloned(),
        _ => None,
    };
    link.send(msg)
        .await
        .map_err(|e| anyhow!("send failed: {e}"))?;

    // No deadline: what is awaited is the command's acknowledgement, not the
    // work it may start.
    loop {
        let Some(bytes) = link.recv().await else {
            anyhow::bail!("server disconnected");
        };
        let event = match decode_server(&bytes)? {
            ServerMessage::Event(event) => event,
            // A kind this build does not know: printed as it arrived, never
            // attributed to a session; `--json` gets the bytes.
            ServerMessage::Unknown { event, .. } => {
                println!("{}", serde_json::to_string(&event)?);
                continue;
            }
            // The greeting and the refusal are the handshake's, not this answer's.
            ServerMessage::Hello { .. } | ServerMessage::Denied { .. } => continue,
        };
        // Feed the client too: a one-shot `set_session` still leaves a sticky
        // `session` behind.
        client.handle_event(event.clone(), &mut printer);
        // The event *is* the output; JSON lines survive `jq` and a pipe.
        if prints(watched.as_deref(), &event) {
            println!("{}", serde_json::to_string(&event)?);
        }
        if let Event::CmdResult {
            id: rid,
            ok,
            message,
            ..
        } = &event
        {
            if *rid == id {
                return if *ok {
                    Ok(())
                } else {
                    anyhow::bail!("{message}")
                };
            }
        }
    }
}

/// Whether a one-shot command prints this event.
///
/// # Invariant
///
/// A command that named a session prints that session's events and the ones that
/// name no session; another session's stream is not its answer, and its count
/// would disagree with what was printed. A command that named no session prints
/// everything. See `docs/notes/cli-bin-gongga.md`.
fn prints(watched: Option<&str>, event: &Event) -> bool {
    watched.is_none_or(|name| event.session().is_none_or(|s| s == name))
}

/// Headless: the commands the server publishes, then exit.
///
/// # Invariant
///
/// The list is the greeting's, never a copy kept here: a script can register a
/// command, and a front-end that named the built-in six would hide the seventh.
pub async fn list_commands(args: &Args) -> anyhow::Result<()> {
    let (client, link) = connect(args).await?;
    let commands = client.commands();
    link.goodbye().await;
    println!(
        "{} command(s); run one with `gongga <command> <args>`; each takes line says its shape.",
        commands.len()
    );
    for command in commands {
        // Who runs it is the reader's business: a client command is the
        // operator's own, and a script's is not built in.
        let target = match command.target {
            protocol::CommandTarget::Server => String::new(),
            protocol::CommandTarget::ServerScript => " [script]".to_string(),
            protocol::CommandTarget::Client => " [client-side]".to_string(),
        };
        println!(
            "{}{} -- {}\n    takes: {}",
            command.name,
            target,
            command.description,
            commands::signature(Some(&command)),
        );
    }
    Ok(())
}

// Connection

/// A resolved address and key, fixed for the life of the process.
///
/// # Invariant
///
/// What to dial, and nothing about who may stop the server: a client reads a
/// server, and only the process that started one decides its lifetime. See
/// `docs/notes/cli-lib.md`.
#[derive(Clone)]
pub struct Endpoint {
    addr: SocketAddr,
    key: String,
}

/// The address this client would dial, without needing its key.
pub fn server_of(conn: &Connection) -> anyhow::Result<SocketAddr> {
    resolve_server(conn)
}

pub fn endpoint(args: &Connection) -> anyhow::Result<Endpoint> {
    let addr = resolve_server(args)?;
    let key = resolve_key(args)
        .ok_or_else(|| anyhow!("no connection key: pass --key or set GONGGA_KEY"))?;
    Ok(Endpoint { addr, key })
}

/// Why a dial did not produce a link.
pub enum DialFailure {
    /// Asking again cannot help: wrong key, malformed address.
    Fatal(anyhow::Error),
    /// The address refused the datagram: nothing is listening there -- the one
    /// failure a server start answers.
    NoListener(anyhow::Error),
    /// Worth retrying: the server is not listening right now.
    Temporary(anyhow::Error),
}

impl DialFailure {
    pub fn message(&self) -> String {
        match self {
            DialFailure::Fatal(e) | DialFailure::NoListener(e) | DialFailure::Temporary(e) => {
                e.to_string()
            }
        }
    }

    pub fn is_fatal(&self) -> bool {
        matches!(self, DialFailure::Fatal(_))
    }

    /// Whether starting a server where this dial went is what would fix it.
    pub fn nothing_listening(&self) -> bool {
        matches!(self, DialFailure::NoListener(_))
    }

    pub fn into_error(self) -> anyhow::Error {
        match self {
            DialFailure::Fatal(e) | DialFailure::NoListener(e) | DialFailure::Temporary(e) => e,
        }
    }
}

/// Dials, authenticates, and waits for the greeting; binds a fresh local port.
///
/// See `docs/notes/cli-lib.md`.
pub async fn dial(endpoint: Endpoint) -> Result<(Link, Greeting), DialFailure> {
    let socket = Arc::new(
        UdpSocket::bind("0.0.0.0:0")
            .await
            .map_err(|e| DialFailure::Temporary(e.into()))?,
    );
    let channel = protocol::connect(socket, endpoint.addr, ChannelConfig::default())
        .await
        .map_err(|e| DialFailure::Temporary(anyhow!("{e}")))?;
    let (wire, mut stream) = channel.split();

    let hello = encode_client(&ClientMessage::Hello {
        client: client_info(),
        key: endpoint.key.clone(),
    })
    .map_err(|e| DialFailure::Fatal(e.into()))?;
    wire.send(hello)
        .await
        .map_err(|e| DialFailure::Temporary(anyhow!("handshake failed: {e}")))?;

    // The greeting confirms the handshake and downloads the catalog.
    let greeting = tokio::time::timeout(HANDSHAKE_TIMEOUT, stream.recv())
        .await
        .map_err(|_| {
            DialFailure::Temporary(anyhow!(
                "no response from {} within {HANDSHAKE_TIMEOUT:?}",
                endpoint.addr
            ))
        })?;
    let Some(greeting) = greeting else {
        // A channel that ends before a greeting ended on one of two facts, and
        // the socket is the one that knows which.
        return Err(if stream.refused() {
            DialFailure::NoListener(anyhow!("nothing is listening at {}", endpoint.addr))
        } else {
            DialFailure::Temporary(anyhow!(
                "{} closed the connection during the handshake",
                endpoint.addr
            ))
        });
    };

    let msg = decode_server(&greeting)
        .context("decoding server greeting")
        .map_err(DialFailure::Temporary)?;
    if let ServerMessage::Denied { reason } = &msg {
        return Err(DialFailure::Fatal(anyhow!(
            "server refused the connection: {reason}"
        )));
    }
    Ok((Link { sink: wire, stream }, Greeting(msg)))
}

/// The server's greeting, already decoded and opaque to a front-end.
pub struct Greeting(ServerMessage);

impl Greeting {
    /// The pid of the server that sent this: who is answering the dial.
    pub fn pid(&self) -> u32 {
        match &self.0 {
            ServerMessage::Hello { server, .. } => server.pid,
            _ => 0,
        }
    }

    /// The watchdog the answering server runs under, when one started it.
    ///
    /// # Invariant
    ///
    /// A front-end that spawned a server holds the watchdog's pid, so "is this
    /// mine" is answered by either pid -- and a stop that kills only the one
    /// answering is undone by the watchdog.
    pub fn supervisor(&self) -> Option<u32> {
        match &self.0 {
            ServerMessage::Hello { server, .. } => server.supervisor,
            _ => None,
        }
    }

    /// The pid whose lifetime that server shares: closing it stops the server.
    pub fn owner(&self) -> Option<u32> {
        match &self.0 {
            ServerMessage::Hello { server, .. } => server.owner,
            _ => None,
        }
    }

    /// Brings a client up to date: catalog, session list, model; this is the
    /// resync.
    pub fn apply(self, client: &mut Client, sink: &mut dyn Sink) {
        client.handle(self.0, sink);
    }
}

/// The client's own state, built without touching the network.
pub fn build_client(args: &Args) -> Client {
    let home = home_dir(args.conn.home.as_deref());
    let catalog = Catalog::new();
    let sticky = Sticky::load(&home);
    Client::new(catalog, sticky)
}

/// Connects and returns a client that already knows the server's catalog.
/// One-shot only: it never reconnects.
async fn connect(args: &Args) -> anyhow::Result<(Client, Link)> {
    // One-shot mode's caller is a script: add the usual cause to the words.
    let (link, greeting) = match dial(endpoint(&args.conn)?).await {
        Ok(pair) => pair,
        Err(failure) if failure.is_fatal() => return Err(failure.into_error()),
        Err(failure) => {
            return Err(anyhow!(
                "{}; is a server running? Start one with `{} serve <port>`",
                failure.message(),
                typed_name()
            ));
        }
    };
    let mut client = build_client(args);
    greeting.apply(&mut client, &mut NullSink::default());
    Ok((client, link))
}

/// Identifies this client to the server.
///
/// # Invariant
///
/// No lifetime of anybody's: a hello says who is reading the server, and never
/// who may stop it. See `docs/notes/cli-lib.md`.
fn client_info() -> ClientInfo {
    ClientInfo {
        version: env!("CARGO_PKG_VERSION").to_string(),
        build: protocol::BUILD.to_string(),
        label: format!("cli/pid-{}", std::process::id()),
        // Only this process knows whose shell it is: `subscribe` needs it.
        session: calling_session(),
    }
}

/// The session whose shell started this process, if any.
///
/// # Invariant
///
/// The environment only, never a sticky argument: `--session` names who a command
/// acts on, and this is who ran it.
fn calling_session() -> Option<String> {
    std::env::var("GONGGA_SESSION")
        .ok()
        .map(|name| name.trim().to_string())
        .filter(|name| !name.is_empty())
}

fn home_dir(explicit: Option<&Path>) -> PathBuf {
    explicit
        .map(Path::to_path_buf)
        .or_else(|| std::env::var_os("GONGGA_HOME").map(PathBuf::from))
        .or_else(|| dirs::home_dir().map(|h| h.join(".gongga")))
        .unwrap_or_else(|| PathBuf::from(".gongga"))
}

/// The process environment, injected so precedence can be tested.
type Env = dyn Fn(&str) -> Option<String>;

fn process_env(key: &str) -> Option<String> {
    std::env::var(key).ok().filter(|v| !v.is_empty())
}

/// `--server`, then `GONGGA_SERVER`, then `[server] bind` in config.toml.
fn resolve_server(args: &Connection) -> anyhow::Result<SocketAddr> {
    resolve_server_with(args, &process_env)
}

fn resolve_server_with(args: &Connection, env: &Env) -> anyhow::Result<SocketAddr> {
    if let Some(s) = &args.server {
        return s.parse().with_context(|| format!("parsing --server {s:?}"));
    }
    if let Some(s) = env("GONGGA_SERVER") {
        return s
            .parse()
            .with_context(|| format!("parsing GONGGA_SERVER {s:?}"));
    }
    // What `connect_server` said outlives the process that said it.
    if let Some(addr) = commands::Remembered::load(&home_dir(args.home.as_deref())) {
        return Ok(addr);
    }
    if let Some(bind) = config_value(args, |c| c.server.as_ref().and_then(|s| s.bind.clone())) {
        return bind
            .parse()
            .with_context(|| format!("parsing bind {bind:?}"));
    }
    Ok(DEFAULT_SERVER.parse().unwrap())
}

/// The variable a server reads a key from, and a client looks one up in.
pub const KEY_ENV: &str = "GONGGA_KEY";

/// `--key`, then `GONGGA_KEY`, then what the home itself carries.
pub(crate) fn resolve_key(args: &Connection) -> Option<String> {
    resolve_key_with(args, &process_env)
}

fn resolve_key_with(args: &Connection, env: &Env) -> Option<String> {
    if let Some(k) = &args.key {
        return Some(k.clone());
    }
    env(KEY_ENV).or_else(|| key_from_home(&home_dir(args.home.as_deref())))
}

/// Whether two spellings name one directory: one may be relative.
fn same_home(server: &str, mine: &Path) -> bool {
    let theirs = Path::new(server);
    let theirs = theirs
        .canonicalize()
        .unwrap_or_else(|_| theirs.to_path_buf());
    let mine = mine.canonicalize().unwrap_or_else(|_| mine.to_path_buf());
    theirs == mine
}

/// The key this process would present to the server of `home`.
pub(crate) fn key_of(home: &Path) -> Option<String> {
    process_env("GONGGA_KEY").or_else(|| key_from_home(home))
}

/// What a home says its key is: `server.key` first, then a `[server] key` a home
/// written before that still has.
///
/// # Invariant
///
/// One reader for both questions -- "what do I present" and "is anything
/// there" -- so the two can never disagree about which key a home means.
pub(crate) fn key_from_home(home: &Path) -> Option<String> {
    let written = std::fs::read_to_string(home.join("server.key"))
        .ok()
        .map(|t| t.trim().to_string())
        .filter(|v| !v.is_empty());
    written.or_else(|| config_in(home, |c| c.server.as_ref().and_then(|s| s.key.clone())))
}

#[derive(serde::Deserialize)]
struct FileConfig {
    server: Option<FileServer>,
}

#[derive(serde::Deserialize)]
struct FileServer {
    bind: Option<String>,
    key: Option<String>,
}

fn config_value(args: &Connection, pick: impl Fn(&FileConfig) -> Option<String>) -> Option<String> {
    config_in(&home_dir(args.home.as_deref()), pick)
}

fn config_in(home: &Path, pick: impl Fn(&FileConfig) -> Option<String>) -> Option<String> {
    let text = std::fs::read_to_string(home.join("config.toml")).ok()?;
    let cfg: FileConfig = toml::from_str(&text).ok()?;
    pick(&cfg)
}
