//! `gongga-client`: the client library and command surface.
//!
//! See `docs/notes/client-lib.md`.

pub mod catalog;
pub mod commands;
pub mod complete;
pub mod server;
pub mod skill_command;

pub use crate::catalog::Catalog;
pub use crate::complete::{
    Candidate, Completer, Completion, Offered, SharedCompleter, token_bounds,
};
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
    SERVER_STOP, ServerMessage, SessionInfo, SessionStatus, ToolAnswerEvent, TurnOutcome,
    decode_server, encode_client,
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
    /// Harness home holding config.toml and tui.toml. Defaults to `~/.gongga`.
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
    /// Stop a server of another build by pid instead of asking it to cancel.
    #[arg(long, global = true)]
    pub force: bool,
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
                "--force",
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
    /// Sessions drawn with a header and their events, in the split's own order.
    ///
    /// # Invariant
    ///
    /// The order the sessions were made in, not the list's: a pane is where the
    /// operator is reading, so a turn that ends elsewhere must not move it.
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
/// See `docs/notes/client-lib.md`.
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
    /// click sets. See `docs/notes/client-lib.md`.
    fn sticky_session(&mut self, _name: Option<&str>) {}
    /// A command's terminal outcome, by id.
    ///
    /// # Invariant
    ///
    /// Told for every answer, mine or not: a front-end that only watches its own
    /// still has to know which of its lines the server took. What it does with
    /// the news is its own -- SPEC's sticky follows an accepted `set_session`,
    /// and that entry is the front-end's to keep.
    fn settled(&mut self, _id: u64, _ok: bool) {}

    /// Every row of `session` up to `seq` has been handed over.
    ///
    /// # Invariant
    ///
    /// Told after the events that drew the row, so a front-end stamping its journal
    /// here states a fact about its own window -- which is what
    /// `SessionInfo::transcript` is later compared against.
    fn transcript(&mut self, _session: &str, _created: i64, _seq: i64) {}
    /// `window` is about to be refilled: drop what no stored row accounts for.
    ///
    /// # Invariant
    ///
    /// Only the tail. Everything up to the window's stamp was drawn from rows that
    /// still exist; keeping the half-written block would show it twice.
    fn rewind(&mut self, _window: &str) {}
    /// The rows handed over from here on are older than the ones `window` holds.
    ///
    /// # Invariant
    ///
    /// A page of the past is drawn where the transcript puts it -- above what the
    /// window already holds -- and nothing is dropped: the page is the rows the
    /// transcript has just before the window's oldest one.
    fn older(&mut self, _window: &str) {}
    /// A page of older rows landed, so `window` holds every row from `floor` up.
    ///
    /// # Invariant
    ///
    /// Nothing is restamped: a page of the past does not move the row the window
    /// ends at, so `drawn` is the number it already had. `floor` is the oldest row
    /// the window holds, and the next page above is counted from it.
    fn landed(&mut self, _window: &str, _created: i64, _drawn: i64, _floor: i64) {}
    /// A session stopped: its turn ended, and it waits for the operator.
    ///
    /// # Invariant
    ///
    /// Told once per turn that ended, and never for a turn that is still
    /// running: a front-end that shows it and one that ignores it hear the same
    /// fact. See `docs/notes/client-lib.md`.
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
    /// Where `skills/` is read. Nothing is written here: client state is the window's.
    home: PathBuf,
    /// Which sessions the front-end wants shown beside the list.
    ///
    /// # Invariant
    ///
    /// A view the front-end dictates: this layer keeps the names so the layout can
    /// answer with them, and never sets one itself. Client state is the window's.
    shown: Vec<String>,
    /// Calls that streamed their arguments, by id: their structure fills that block.
    calls: HashSet<String>,
    /// Which window each accepted command's result belongs to, from its echo.
    results: HashMap<u64, String>,
    /// Command lines sent and not answered yet, oldest first, by command id.
    ///
    /// # Invariant
    ///
    /// The words are the operator's until the server answers them: a line leaves
    /// this list on its `cmd_result`, and a link that ends first gives it back
    /// through `set_offline`. See `docs/notes/client-lib.md`.
    unconfirmed: Vec<(u64, String)>,
    sessions: Vec<SessionInfo>,
    /// The session list this client was handed before it ever dialled, from the
    /// window's own record of the last one it drew.
    ///
    /// # Invariant
    ///
    /// A prefix of nothing and no claim about a transcript: the rows carry no
    /// `created_at` and no `transcript`, so nothing here can be compared with a
    /// server -- they exist only so a window standing on no link has a list to
    /// draw. A greeting is the validation, so it clears them; a session the server
    /// drops afterwards is pruned rather than kept. See `docs/notes/tui-roster.md`.
    seeded: Vec<Listed>,
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
    /// Set when the stream skipped events this client will never be handed.
    ///
    /// # Invariant
    ///
    /// The hole is the whole stream's, never one session's: a lost event may be a
    /// session's row or the name that says a session is gone, so nothing in this
    /// layer can work out what is missing. A greeting is the only whole picture, so
    /// the front-end reads this and dials again. See `docs/notes/client-lib.md`.
    holed: bool,
    /// The server said it is finishing a `cancel`. See `docs/notes/client-lib.md`.
    draining: bool,
    /// Another build or home: part of the facts, which no note can wipe.
    build_note: Option<String>,
    home_note: Option<String>,
    /// SPEC's `active_quota` row, as the last step's reading spelled it.
    quota: Option<QuotaRow>,
    /// Per session: where the server's transcript stands, and what this client
    /// has drawn of it. See [`Sync`].
    sync: HashMap<String, Sync>,
    /// The position of the last event applied on this link: a jump is a hole.
    cursor: Option<u64>,
}

/// What this client says it has drawn of one session.
///
/// # Invariant
///
/// The front-end's declaration and nothing else: this layer never reads a journal, so
/// anything but its word here would be a guess about what a window holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Drawn {
    /// Nothing declared yet: nothing may be fetched on its behalf.
    Unknown,
    /// The session, and the row its window ends at.
    At { created: i64, seq: i64 },
}

/// One session's transcript, as the two sides state it.
///
/// # Invariant
///
/// Two numbers, neither derived from the other: `server` is the last thing a
/// `SessionInfo` or `Transcript` said, `drawn` is what the front-end declared.
/// Answering "in sync" from anything but their comparison is a guess.
#[derive(Debug)]
struct Sync {
    /// Its `created_at`: a name handed out again is a different transcript.
    created: i64,
    /// The last row the server says it has written.
    server: i64,
    /// What the front-end says its own journal holds.
    drawn: Drawn,
    /// The refill in flight, when one is. See [`Fill`].
    fill: Option<Fill>,
    /// The oldest row of the transcript this window holds, when it holds any.
    ///
    /// # Invariant
    ///
    /// The other end of `drawn`: what `drawn` is the newest row of, this is the
    /// oldest, and a page above it is the rows `floor - room .. floor`. `None` means
    /// the window holds no row this client can name, so nothing is owed above it.
    floor: Option<i64>,
    /// Whether a page has been read for this window at all.
    ///
    /// # Invariant
    ///
    /// Not the same as "in sync": a window that was never given a page holds
    /// nothing, and the first thing it is owed is the *tail* -- the rows a pane can
    /// draw -- not the transcript. See `docs/notes` on lazy retrieval.
    loaded: bool,
    /// Set when a refill was refused, so a drawing window cannot spin.
    ///
    /// # Invariant
    ///
    /// Cleared by a greeting and nothing else: a refusal answers one range, not a moment.
    blocked: bool,
}

/// A refill in flight: what the window is owed, and what arrived meanwhile.
///
/// # Invariant
///
/// The reply is a snapshot as of `as_of`, so a buffered event is either inside it
/// (`seq <= as_of`, dropped) or news (`seq > as_of`, applied after). That is the join:
/// no gap for want of a resend, no line twice for want of a number.
#[derive(Debug)]
struct Fill {
    /// The command's id, so its answer closes this fill.
    id: u64,
    /// What the bus sent while the answer was on its way.
    buffered: Vec<(u64, Event)>,
    /// Whether the whole window is re-read, rather than the tail after its stamp.
    whole: bool,
    /// Whether the page is the rows *above* what the window holds, not below.
    ///
    /// # Invariant
    ///
    /// A page of the past never rewinds and never restamps: the tail stays as it is
    /// and the page lands above it. See [`Client::ask_older`].
    older: bool,
    /// Where this page's oldest row comes from, once it is answered.
    floor: First,
    /// Whether the front-end was told to make room: once per fill, not per event.
    rewound: bool,
}

/// The oldest row a page will have, as the request names it.
///
/// # Invariant
///
/// A range spelled `-N:` is resolved by the server against the transcript as it
/// stands when it answers, so its first row is only known then: `Counted` is the
/// one form that cannot be read off the words sent. Anything else names its first
/// row, and that is the number the page's floor will be.
#[derive(Debug, Clone, Copy)]
enum First {
    /// The range's start, as written.
    Named(i64),
    /// Counted back from the answered transcript's last row, by this many rows.
    Counted(i64),
}

impl First {
    /// The oldest row a page of this width has, given where the transcript ended.
    fn at(self, last_seq: i64) -> i64 {
        match self {
            First::Named(first) => first.max(1),
            First::Counted(room) => (last_seq - room + 1).max(1),
        }
    }
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

    /// The command row, for a front-end that answered a line itself.
    ///
    /// # Invariant
    ///
    /// The window naming is this layer's, because only it knows which sessions
    /// exist: an answer goes where a command's answer goes, whatever ran it.
    pub fn say(&self, kind: LineKind, text: &str, sink: &mut dyn Sink) {
        self.row(sink, kind, text);
    }

    /// The echo of a command line, for a front-end that answered it itself.
    ///
    /// # Invariant
    ///
    /// The same window `run_client_command` would have used: a command that names
    /// a session is echoed where that session's lines are read.
    pub fn echo(&self, tokens: &[String], session: Option<&str>, sink: &mut dyn Sink) {
        self.show(sink, &Echo::Command, tokens, session);
    }

    /// Every session the server knows, in its order.
    pub fn sessions(&self) -> &[SessionInfo] {
        &self.sessions
    }

    /// The list the window draws: the server's rows, then the seeded ones.
    ///
    /// # Invariant
    ///
    /// One list, one order, for every reader -- the frame, the completion and the
    /// window names a command is echoed into -- so a seeded row and an announced
    /// one are not two sessions with one name. An announced name wins: the roster is
    /// what the window last knew, and the server is what is.
    /// See `docs/notes/tui-roster.md`.
    pub fn listed(&self) -> Vec<Listed> {
        let mut rows: Vec<Listed> = self
            .sessions
            .iter()
            .rev()
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
            .collect();
        for row in &self.seeded {
            if !rows.iter().any(|held| held.name == row.name) {
                rows.push(row.clone());
            }
        }
        rows
    }

    /// Hands this client the list its window drew last, for a link that is not open
    /// yet.
    ///
    /// # Invariant
    ///
    /// Not a greeting: nothing here says how far any transcript reaches, so no
    /// session is declared drawn and nothing is fetched on its behalf. It buys a
    /// list to draw, and the first greeting takes it away again.
    pub fn seed_listed(&mut self, rows: Vec<Listed>) {
        self.seeded = rows;
    }

    /// The sessions a front-end wants drawn beside the list.
    ///
    /// # Invariant
    ///
    /// A view the front-end owns: this layer keeps the names so the layout can
    /// answer with them, and never decides one itself.
    pub fn set_shown(&mut self, names: Vec<String>) {
        self.shown = names;
    }

    /// What the front-end's own entries mean to a completion.
    ///
    /// # Invariant
    ///
    /// Two facts and no third: the sticky argument names, and the session a bare
    /// message goes to. This layer stores neither; it answers from them.
    pub fn set_sticky_vocabulary(&self, names: &[String], message_session: Option<&str>) {
        self.catalog.set_sticky_names(names);
        self.catalog.set_message_session(message_session);
    }

    /// The arguments of `line` whose value nothing in the catalog accepts.
    pub fn refused_values(&self, line: &str, args: &protocol::Args) -> Vec<(String, String)> {
        self.catalog.refused_values(line, args)
    }

    /// Whether a session goes by `name`: the server's word first, then the row this
    /// client was handed before it dialled.
    ///
    /// # Invariant
    ///
    /// The same names `listed` answers, and the reason it is a membership test rather
    /// than a scan of that list: the command window's name is decided by asking this
    /// in a loop, and a list built per question would be a vector per iteration.
    fn taken(&self, name: &str) -> bool {
        self.sessions.iter().any(|session| session.name == name)
            || self.seeded.iter().any(|row| row.name == name)
    }

    /// The window for a line that names no session, named so no session can take it.
    fn command_window(&self) -> String {
        let mut name = COMMAND_WINDOW.to_string();
        while self.taken(&name) {
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
    /// See `docs/notes/client-lib.md`.
    fn answer_window(&self, window: String) -> String {
        let live = window == self.command_window() || self.taken(&window);
        if live { window } else { self.command_window() }
    }

    /// What a block opens with: the label says which part of the answer it is.
    ///
    /// # Invariant
    ///
    /// The session's name is never in the text: the window draws it, bracketed, in
    /// the list and in its own header. Two places saying it is how `[main] [main]
    /// ...` happened. See `docs/notes/client-lib.md`.
    fn tag(&self, _session: &str, label: &str) -> String {
        label.to_string()
    }

    /// How the window lays itself out: SPEC's UI section, as its lists.
    ///
    /// # Invariant
    ///
    /// Every session the server knows is listed whatever its window -- the list is
    /// where a session is clicked -- and `shown` is the ones drawn beside it. The
    /// two orders differ on purpose: the list by the turn that ended last, the
    /// split by when a session was made, so no pane moves under a reader.
    /// See `docs/notes/tui-tree.md`.
    fn layout(&self) -> Layout {
        Layout {
            listed: self.listed(),
            shown: self.split(),
            command: self.command_window(),
            quota: self.quota.clone(),
        }
    }

    /// The split's order: the sessions drawn beside the list, oldest first.
    ///
    /// # Invariant
    ///
    /// `created_at`, never the turn that ended: a frame the operator is reading
    /// must not be moved by a turn that finished somewhere else. A name the server
    /// no longer has is not drawn, and a name this client remembers while offline
    /// keeps the place it had, last. See `docs/notes/tui-tree.md`.
    fn split(&self) -> Vec<String> {
        let mut held: Vec<&protocol::SessionInfo> = self
            .sessions
            .iter()
            .filter(|s| self.shown.contains(&s.name))
            .collect();
        held.sort_by(|a, b| {
            a.created_at
                .cmp(&b.created_at)
                .then_with(|| a.name.cmp(&b.name))
        });
        let mut names: Vec<String> = held.iter().map(|s| s.name.clone()).collect();
        for row in &self.seeded {
            if self.shown.contains(&row.name) && !names.contains(&row.name) {
                names.push(row.name.clone());
            }
        }
        names
    }

    fn new(catalog: Catalog, home: PathBuf) -> Self {
        // Where relative values are read until a session says: this process's,
        // which the greeting replaces with the server's.
        catalog.set_roots(&home, &std::env::current_dir().unwrap_or_default());
        Self {
            catalog,
            home,
            shown: Vec::new(),
            results: HashMap::new(),
            unconfirmed: Vec::new(),
            calls: HashSet::new(),
            sessions: Vec::new(),
            seeded: Vec::new(),
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
            sync: HashMap::new(),
            cursor: None,
            holed: false,
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

    /// Whether the stream skipped events, once per hole.
    ///
    /// # Invariant
    ///
    /// The front-end's cue to dial again, because a greeting is the only thing that
    /// rebuilds a session list. Nothing here can repair itself: what was published
    /// in the hole was published once.
    pub fn take_hole(&mut self) -> bool {
        std::mem::take(&mut self.holed)
    }

    /// The first id this client will use.
    ///
    /// # Invariant
    ///
    /// Hard for separate processes to collide. Command results are connection-
    /// local, but distinct ids also keep logs and the rows they answer clear.
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
                cursor,
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
                self.catalog.set_roots(&self.home, Path::new(&server.cwd));
                // A greeting *is* the recovery: it carries the catalog and
                // sessions, so a redial comes up like a fresh client.
                self.connected = true;
                // A greeting opens a stream: a fill left set would never settle.
                for sync in self.sync.values_mut() {
                    sync.fill = None;
                    sync.blocked = false;
                }
                self.attempts = 0;
                self.draining = server.draining;
                // Where this link's stream starts.
                self.cursor = Some(cursor);
                // A greeting carries the whole list, so it answers a hole as well
                // as it opens a link.
                self.holed = false;
                // The announcement is the facts: the model and the session count.
                // A warning belongs beside them.
                self.build_note = (!server.build.is_empty() && server.build != protocol::BUILD)
                    .then(|| format!("build {}", server.build));
                let mine = self.home.clone();
                self.home_note = (!server.home.is_empty()
                    && !same_home(&server.home, mine.as_path()))
                .then(|| format!("home {}", server.home));
                self.refresh(sink);
            }
            ServerMessage::Denied { reason } => {
                sink.note(LineKind::Error, &format!("connection refused: {reason}"));
            }
            ServerMessage::Event { seq, event } => self.handle_delta(seq, event, sink),
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

    /// One event and its place in the stream.
    ///
    /// # Invariant
    ///
    /// A number that is not the one after the last is a hole, and nothing here
    /// invents what it held: the whole stream is marked holed, because what was
    /// missed is not one session's rows -- a deletion is one name and it may be the
    /// name that was lost. [`Client::take_hole`] is how a front-end hears it.
    ///
    /// While a refill is in flight its events wait here. See [`Fill`].
    fn handle_delta(&mut self, seq: Option<u64>, event: Event, sink: &mut dyn Sink) {
        let Some(seq) = seq else {
            // A command's own answer: no position in the stream, but a refill's
            // events do arrive this way.
            self.handle_answer(event, sink);
            return;
        };
        let skipped = self.cursor.is_some_and(|last| seq > last + 1);
        self.cursor = Some(self.cursor.map_or(seq, |last| last.max(seq)));
        if skipped {
            self.holed = true;
        }
        if let Some(session) = event.session()
            && let Some(sync) = self.sync.get_mut(session)
            && let Some(fill) = sync.fill.as_mut()
        {
            fill.buffered.push((seq, event));
            return;
        }
        self.handle_event(event, sink);
    }

    /// One event that is this connection's answer rather than the stream's.
    ///
    /// # Invariant
    ///
    /// The first event of a refill is what says the window must make room: the
    /// rows that follow are the window's whole tail, so the half-written block
    /// that no stored row accounts for must go before the first of them lands.
    fn handle_answer(&mut self, event: Event, sink: &mut dyn Sink) {
        if let Some(session) = event.session()
            && let Some(sync) = self.sync.get_mut(session)
            && let Some(fill) = sync.fill.as_mut()
            && !fill.rewound
        {
            fill.rewound = true;
            let window = session.to_string();
            match (fill.whole, fill.older) {
                (true, _) => sink.forget(&window),
                // A page of the past lands above the window, so nothing goes.
                (false, true) => sink.older(&window),
                (false, false) => sink.rewind(&window),
            }
        }
        self.handle_event(event, sink);
    }

    /// One event from the server, given to the front-end whatever window it names.
    ///
    /// # Invariant
    ///
    /// Nothing here is dropped for belonging to a hidden session:
    /// `set_render_session` changes what a frame draws, never what the client is
    /// told. A front-end that records what it is handed -- the TUI's journal --
    /// therefore keeps a hidden session's rows, and showing it again draws them.
    /// See `docs/notes/client-lib.md`.
    fn handle_event(&mut self, event: Event, sink: &mut dyn Sink) {
        match event {
            Event::Cmd { id, name, args } => {
                // Neither the completion's questions nor a refill is the operator's
                // line: the window does not print what healed it.
                if self.quiet.contains_key(&id) || self.is_fill(id) {
                    return;
                }
                // The session is the argument's kind, never its spelling: see
                // `docs/notes/client-lib.md`, `Client::command_line`.
                let session = self.session_named(&name, &args);
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
                self.unconfirmed.retain(|(other, _)| *other != id);
                // A refill's answer says how far its transcript reaches, nothing else.
                if self.settle_fill(id, ok, &message, data.as_ref(), sink) {
                    return;
                }
                // Before the row: an accepted command's news reaches the front-end.
                sink.settled(id, ok);
                if let Some(remote) = self.quiet.remove(&id) {
                    match (ok, data.as_ref()) {
                        (true, Some(named)) => self.fill_listing(named),
                        // Asked again next time: a refusal is not an answer.
                        _ => self.catalog.fail_paths(&remote),
                    }
                    return;
                }
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
            Event::Session { session } => {
                self.apply_session(session, sink);
                self.refresh(sink);
            }
            Event::SessionGone { name } => {
                self.drop_session(&name, sink);
                self.refresh(sink);
            }
            // Where a session's transcript ends: a stamp, never a line of its own.
            Event::Transcript { session, seq } => {
                self.announce_transcript(&session, seq, sink);
            }
            // An edited roster: silent, and the catalog follows it.
            Event::Models { models } => self.catalog.set_models(models),
            Event::Commands { commands } => self.catalog.set_commands(commands),
            Event::Status {
                session,
                status,
                detail,
                outcome,
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
                // A turn somebody stopped is not a stop to announce: see TurnOutcome.
                if status != SessionStatus::Running && outcome != Some(TurnOutcome::Cancelled) {
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
            // The row `cancel` writes first: the notice, and who asked.
            Event::Record { kind, .. } if kind == SERVER_STOP => {
                if !self.draining {
                    self.draining = true;
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
        // The operator's own words, replayed: a line, exactly as the echo that
        // wrote them live put it there.
        if e.part == ResponsePart::Input {
            let window = e.session.clone();
            sink.line(&window, LineKind::User, &e.text);
            return;
        }
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
            // Handled above; the block path never sees one.
            ResponsePart::Input => (LineKind::User, ""),
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
    /// on the deleted one's lines. See `docs/notes/client-lib.md`.
    fn set_sessions(&mut self, sessions: Vec<SessionInfo>, sink: &mut dyn Sink) {
        for old in &self.sessions {
            if !sessions.iter().any(|new| new.name == old.name) {
                sink.forget(&old.name);
                self.sync.remove(&old.name);
            }
        }
        // As published: a value completed for a session belongs to that
        // session's directory and machine.
        self.catalog.set_sessions(sessions.clone());
        for row in &sessions {
            self.stamp_server(row);
        }
        self.sessions = sessions;
        // The greeting is the validation: a name it leaves out is not drawn.
        self.seeded.clear();
    }

    /// Records what the server says about one session's transcript.
    ///
    /// # Invariant
    ///
    /// A session whose `created_at` moved is a different session under a reused
    /// name, and what the window drew belongs to the old one: the declaration is
    /// dropped. That `created_at` check is why the stamp is a pair and not a
    /// number -- `seq` alone compares two unrelated transcripts happily.
    fn stamp_server(&mut self, row: &SessionInfo) {
        let entry = self.sync.entry(row.name.clone()).or_insert(Sync {
            created: row.created_at,
            server: row.transcript,
            drawn: Drawn::Unknown,
            fill: None,
            floor: None,
            loaded: false,
            blocked: false,
        });
        if entry.created != row.created_at {
            entry.created = row.created_at;
            entry.drawn = Drawn::Unknown;
            entry.fill = None;
            entry.floor = None;
            entry.loaded = false;
            entry.blocked = false;
        }
        entry.server = row.transcript;
    }

    /// One session row arrived: created, or changed.
    fn apply_session(&mut self, row: SessionInfo, sink: &mut dyn Sink) {
        let added = !self.sessions.iter().any(|s| s.name == row.name);
        // The server has just spoken about this name: the window's own row is the
        // older word and stops standing in for it.
        self.seeded.retain(|held| held.name != row.name);
        self.stamp_server(&row);
        // A name never listed here has no journal either: the whole transcript is owed.
        if added && let Some(sync) = self.sync.get_mut(&row.name) {
            let created = sync.created;
            sync.drawn = Drawn::At { created, seq: 0 };
        }
        match self.sessions.iter_mut().find(|s| s.name == row.name) {
            Some(held) => *held = row,
            None => self.sessions.push(row),
        }
        // A turn that ended moves its session: put the row back where `ended_at` says.
        self.sessions.sort_by(newest_turn_first);
        self.catalog.set_sessions(self.sessions.clone());
        self.refresh(sink);
    }

    /// One session left: its window is emptied and its window's stamp goes.
    fn drop_session(&mut self, name: &str, sink: &mut dyn Sink) {
        self.sessions.retain(|s| s.name != name);
        self.seeded.retain(|held| held.name != name);
        self.sync.remove(name);
        self.catalog.set_sessions(self.sessions.clone());
        sink.forget(name);
        self.refresh(sink);
    }

    /// One transcript grew: the server's side of the comparison, and the
    /// front-end's chance to stamp what it just drew.
    fn announce_transcript(&mut self, session: &str, seq: i64, sink: &mut dyn Sink) {
        let mut stamp = None;
        if let Some(sync) = self.sync.get_mut(session) {
            // A current window stays current: the events that drew the row came first.
            let current = matches!(
                sync.drawn,
                Drawn::At { created, seq } if created == sync.created && seq == sync.server
            );
            sync.server = seq;
            if current {
                sync.drawn = Drawn::At {
                    created: sync.created,
                    seq,
                };
                stamp = Some((sync.created, seq));
            }
        }
        if let Some(row) = self.sessions.iter_mut().find(|s| s.name == session) {
            row.transcript = seq;
        }
        if let Some((created, seq)) = stamp {
            sink.transcript(session, created, seq);
        }
    }

    /// Whether `id` is a refill this layer asked for.
    fn is_fill(&self, id: u64) -> bool {
        self.sync
            .values()
            .any(|sync| sync.fill.as_ref().is_some_and(|fill| fill.id == id))
    }

    /// Closes a refill's answer, so what arrived meanwhile can be applied on top.
    ///
    /// # Invariant
    ///
    /// An event is inside the reply when the answer replays it -- [`restated`] --
    /// or when it is numbered at or below the `as_of` the answer is a picture of;
    /// everything else is news and is applied in order. A refusal leaves the window
    /// behind, never "current", and restates nothing.
    fn settle_fill(
        &mut self,
        id: u64,
        ok: bool,
        message: &str,
        data: Option<&serde_json::Value>,
        sink: &mut dyn Sink,
    ) -> bool {
        let Some(name) = self
            .sync
            .iter()
            .find(|(_, sync)| sync.fill.as_ref().is_some_and(|f| f.id == id))
            .map(|(name, _)| name.clone())
        else {
            return false;
        };
        let field = |key: &str| {
            data.and_then(|d| d.get(key))
                .and_then(|v| v.as_i64())
                .unwrap_or(0)
        };
        let last_seq = field("last_seq");
        let as_of = data.and_then(|d| d.get("as_of")).and_then(|v| v.as_u64());
        let Some(sync) = self.sync.get_mut(&name) else {
            return true;
        };
        let Some(fill) = sync.fill.take() else {
            return true;
        };
        // It was never the operator's line, so no answer of it is owed anywhere.
        self.results.remove(&id);
        if !ok || as_of.is_none() {
            // Refused, not pending: the window says why once and waits for a new link.
            if let Some(sync) = self.sync.get_mut(&name) {
                sync.blocked = true;
            }
            sink.note(
                LineKind::Error,
                &format!("{name}: its transcript could not be fetched: {message}"),
            );
            // A refusal is not a picture of anything, so not one of the events that
            // waited behind it is in it.
            for (_, event) in fill.buffered {
                self.handle_event(event, sink);
            }
            return true;
        }
        let sync_created = sync.created;
        let as_of = as_of.unwrap_or_default();
        // What the answer covers goes: a replayed row, or one older than its
        // picture. See `docs/notes/client-lib.md`.
        let (owed, restated): (Vec<_>, Vec<_>) = fill
            .buffered
            .into_iter()
            .partition(|(seq, event)| *seq > as_of || !restated(event));
        drop(restated);
        sync.server = last_seq;
        let floor = fill.floor.at(last_seq);
        if fill.older {
            // A page of the past: the row the window ends at is the one it had, and
            // the floor moves up to where this page starts.
            sync.floor = Some(floor);
            let drawn = match sync.drawn {
                Drawn::At { seq, .. } => seq,
                Drawn::Unknown => 0,
            };
            sink.landed(&name, sync_created, drawn, floor);
        } else {
            // The tail: the window now ends where the answer does, and the page it
            // was given says which rows it holds.
            sync.loaded = true;
            sync.drawn = Drawn::At {
                created: sync.created,
                seq: last_seq,
            };
            sync.floor = Some(sync.floor.map_or(floor, |held| held.min(floor)));
            sink.landed(&name, sync_created, last_seq, floor);
        }
        for (_, event) in owed {
            self.handle_event(event, sink);
        }
        self.refresh(sink);
        true
    }

    /// The status line: server facts only, never a "current session".
    /// See `docs/notes/client-lib.md`.
    pub fn refresh(&self, sink: &mut dyn Sink) {
        sink.layout(&self.layout());
        let mut facts = if self.connected && self.draining {
            // Connected and stopping: the one thing the reader cannot work out
            // from silence, said outright.
            "stopping — its turns were interrupted and it is leaving".to_string()
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
    ///
    /// # Invariant
    ///
    /// Answers with the lines that had no answer, so the front-end can give them
    /// back: a command whose link ended first is not a command the server refused.
    /// The report is a row rather than a note, because a note is wiped by the next
    /// dial's status -- "no answer" must stay readable after the window stopped
    /// looking. See `docs/notes/client-lib.md`.
    pub fn set_offline(&mut self, attempts: u32, sink: &mut dyn Sink) -> Vec<String> {
        self.connected = false;
        self.results.clear();
        self.attempts = attempts;
        // Nothing that was in flight will be answered: the stream it was read
        // from is gone, and the next dial starts a new one.
        self.cursor = None;
        for sync in self.sync.values_mut() {
            sync.fill = None;
        }
        self.refresh(sink);
        let lines: Vec<String> = std::mem::take(&mut self.unconfirmed)
            .into_iter()
            .map(|(_, line)| line)
            .collect();
        if !lines.is_empty() {
            self.row(sink, LineKind::Error, &unanswered(&lines));
        }
        lines
    }

    /// Whether the greeting published this name, which is the only list there is.
    pub fn has_command(&self, name: &str) -> bool {
        self.catalog.command(name).is_some()
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

    /// What the front-end's own journal holds for one session, told once per
    /// declaration.
    ///
    /// # Invariant
    ///
    /// The front-end is the only one who knows this: it is the length of a
    /// journal this layer never reads. Saying it here is what lets the greeting
    /// compare two numbers instead of shipping the transcript.
    pub fn declare_drawn(&mut self, name: &str, created: i64, drawn: i64, floor: Option<i64>) {
        let Some(sync) = self.sync.get_mut(name) else {
            return;
        };
        if sync.created != created {
            sync.drawn = Drawn::Unknown;
            sync.floor = None;
            return;
        }
        sync.drawn = Drawn::At {
            created,
            seq: drawn,
        };
        sync.floor = floor;
    }

    /// The sessions whose window is behind the server, and the row a refill
    /// would start at.
    ///
    /// # Invariant
    ///
    /// Names, never sends: what is behind is decided here, but *when* it is
    /// fetched belongs to the front-end that is about to draw the window. A
    /// session nobody reads costs nothing, which is the whole point of a
    /// greeting that states stamps instead of content.
    pub fn behind(&self) -> Vec<(String, i64)> {
        self.sync
            .keys()
            .filter_map(|name| self.behind_one(name).map(|from| (name.clone(), from)))
            .collect()
    }

    /// Where a refill of one window would start, or `None` when it is current.
    pub fn behind_one(&self, name: &str) -> Option<i64> {
        let sync = self.sync.get(name)?;
        if sync.fill.is_some() || sync.blocked {
            return None;
        }
        match sync.drawn {
            // Nothing declared: nothing owed on the front-end's behalf.
            Drawn::Unknown => None,
            // A reused name is another transcript entirely.
            Drawn::At { created, .. } if created != sync.created => Some(1),
            // Past the server's own end: rows were truncated, so the window
            // holds rows that no longer exist.
            Drawn::At { seq, .. } if seq > sync.server => Some(1),
            Drawn::At { seq, .. } if seq < sync.server => Some(seq + 1),
            Drawn::At { .. } => None,
        }
    }

    /// Declares that this front-end holds nothing of the session: an empty journal,
    /// so the whole transcript is owed.
    ///
    /// # Invariant
    ///
    /// A window with no record is behind by everything, never "in sync": a session
    /// that ran before this window existed is exactly what the operator came to
    /// read. Declaring `0` says so without claiming to know where its record ends.
    pub fn declare_empty(&mut self, name: &str) {
        if let Some(sync) = self.sync.get_mut(name) {
            let created = sync.created;
            sync.drawn = Drawn::At { created, seq: 0 };
            sync.floor = None;
        }
    }

    /// Asks the server for one window's missing rows; `None` when nothing is
    /// owed or there is no link.
    ///
    /// # Invariant
    ///
    /// Never echoes: it is the window's own question, not the operator's line. Its
    /// rows arrive as the events that drew them live, so a refill looks like a stream.
    pub fn ask_transcript(&mut self, name: &str, room: usize) -> Option<ClientMessage> {
        let sync = self.sync.get(name)?;
        if sync.fill.is_some() || sync.blocked {
            return None;
        }
        // One page, never a transcript: the rows a pane can draw, or the tail after.
        let room = room.max(1) as i64;
        let (range, first) = match sync.loaded {
            false if sync.drawn != Drawn::Unknown => (format!("-{room}:"), First::Counted(room)),
            false => return None,
            true => {
                let from = self.behind_one(name)?;
                (format!("{from}:{}", from + room - 1), First::Named(from))
            }
        };
        // Whole only for rows it cannot name: a page appended to those would repeat.
        let whole = !sync.loaded && sync.floor.is_none() && sync.drawn != Drawn::Unknown;
        self.begin_fill(name, range, first, whole, false)
    }

    /// Whether a refill of this window is already on its way.
    ///
    /// # Invariant
    ///
    /// Asked by a caller whose request was refused for that reason alone: a fill
    /// answers the page it was asked for, and the caller's own request waits for it.
    pub fn waiting(&self, name: &str) -> bool {
        self.sync.get(name).is_some_and(|sync| sync.fill.is_some())
    }

    /// Asks the server for the page of rows just above the oldest one the window
    /// holds, sized by what a pane can draw.
    ///
    /// # Invariant
    ///
    /// One page, and only when there is something above the floor: a window whose
    /// floor is the transcript's first row is owed nothing, and asking again would
    /// be a request the answer could not change.
    pub fn ask_older(&mut self, name: &str, room: usize) -> Option<ClientMessage> {
        let sync = self.sync.get(name)?;
        if sync.fill.is_some() || sync.blocked {
            return None;
        }
        let room = room.max(1) as i64;
        let above = sync.floor?.saturating_sub(1);
        if above < 1 {
            return None;
        }
        let first = (above - room + 1).max(1);
        self.begin_fill(
            name,
            format!("{first}:{above}"),
            First::Named(first),
            false,
            true,
        )
    }

    /// Sends one `get_messages` refill and remembers what the window is owed.
    fn begin_fill(
        &mut self,
        name: &str,
        range: String,
        first: First,
        whole: bool,
        older: bool,
    ) -> Option<ClientMessage> {
        if !self.connected || self.catalog.command("get_messages").is_none() {
            return None;
        }
        let id = self.next_id();
        let mut args = protocol::Args::new();
        // The rows a window draws, and where the transcript stood: a refill, not a read.
        args.insert("range".to_string(), range);
        args.insert("session".to_string(), name.to_string());
        args.insert("replay".to_string(), "true".to_string());
        let sync = self.sync.get_mut(name)?;
        sync.fill = Some(Fill {
            id,
            buffered: Vec::new(),
            whole,
            older,
            floor: first,
            rewound: false,
        });
        Some(ClientMessage::Command {
            id,
            name: "get_messages".to_string(),
            args,
        })
    }

    /// Sends one line of prose: SPEC's UI expands it to `send_message`.
    ///
    /// # Invariant
    ///
    /// The session arrives from the front-end, which is where the sticky
    /// `session` entry lives: this layer holds no client state, so it takes the
    /// address it is given and echoes the text, not the command it becomes.
    pub fn message(&mut self, text: &str, session: &str, sink: &mut dyn Sink) -> Submission {
        let spec = self.catalog.command("send_message");
        let name = spec
            .as_ref()
            .and_then(|spec| {
                spec.args
                    .iter()
                    .find(|a| a.kind.as_deref() == Some("session"))
                    .map(|a| a.name.clone())
            })
            .unwrap_or_else(|| "name".to_string());
        self.submit_tokens(
            vec![
                "send_message".to_string(),
                "--message".to_string(),
                text.to_string(),
                format!("--{name}"),
                session.to_string(),
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
        if !self.connected {
            self.row(
                sink,
                LineKind::Error,
                &format!("{head}: not connected to the server, nothing was sent"),
            );
            return Submission::Offline;
        }

        let Some(spec) = self.catalog.command(&head) else {
            // `/name` and a Git Bash-rewritten path get the line that was
            // meant. See `docs/notes/client-lib.md`.
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
        let args = match parse_args(&tokens[1..], Some(&spec)) {
            Ok(a) => a,
            Err(e) => {
                self.row(sink, LineKind::Error, &e.to_string());
                return Submission::Rejected;
            }
        };
        let missing: Vec<&str> = spec
            .args
            .iter()
            .filter(|a| a.required && !args.contains_key(&a.name))
            .map(|a| a.name.as_str())
            .collect();
        if !missing.is_empty() {
            // The window the error belongs to is the session the command
            // names, wherever its spec keeps the kind-`session` argument.
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
        let id = self.next_id();
        // The words stay the operator's until the server answers them: this is
        // what a link that ends first gives back.
        let typed = match &echo {
            Echo::Command => format!("/{}", tokens.join(" ")),
            Echo::Message(text) => (*text).to_string(),
        };
        self.unconfirmed.push((id, typed));
        Submission::Send(ClientMessage::Command {
            id,
            name: head,
            args,
        })
    }

    /// Forgets a command that never left this process: nothing was sent, so there
    /// is nothing the server could answer.
    pub fn never_sent(&mut self, msg: &ClientMessage) {
        if let ClientMessage::Command { id, .. } = msg {
            self.unconfirmed.retain(|(other, _)| other != id);
            // A refill never sent is not in flight: left set, it never settles.
            for sync in self.sync.values_mut() {
                if sync.fill.as_ref().is_some_and(|fill| fill.id == *id) {
                    sync.fill = None;
                }
            }
        }
    }

    /// Applies one operation: the API a window's gestures use.
    ///
    /// # Invariant
    ///
    /// Only the gesture the server answers is here: a window's own state -- which
    /// session is sticky, which are drawn -- is the window's, and asking this
    /// layer to change it would be a second owner of one fact.
    pub fn apply(&mut self, operation: protocol::Operation, sink: &mut dyn Sink) -> Submission {
        let _ = sink;
        match operation {
            protocol::Operation::DeleteSession { .. } => {
                let id = self.next_id();
                Submission::Send(ClientMessage::Operation { id, operation })
            }
            protocol::Operation::StickySession { .. }
            | protocol::Operation::RenderSession { .. } => Submission::Local,
        }
    }

    /// The session a command names, when the catalog says it names one.
    ///
    /// # Invariant
    ///
    /// The kind is the catalog's, not the argument's name: `set_session --name`
    /// and `get_messages --session` bind different names to one idea, and only
    /// the kind says which is the session. A client command names none:
    /// `set_render_session` has a session argument, but says whether that
    /// session is drawn -- following it would undo what it says.
    /// See `docs/notes/client-lib.md`.
    pub fn session_named<'a>(&self, name: &str, args: &'a protocol::Args) -> Option<&'a str> {
        let spec = self.catalog.command(name)?;
        if spec.target != protocol::CommandTarget::Server {
            return None;
        }
        let name = spec
            .args
            .iter()
            .find(|a| a.kind.as_deref() == Some("session"))
            .map(|a| a.name.clone())?;
        args.get(&name).map(String::as_str)
    }
}

/// SPEC's `full` pane title: the measurements, then what the session runs on.
///
/// # Invariant
///
/// Each figure carries SPEC's unit, and one nobody measured is left out rather than
/// shown as zero: `ctx 0` reads as an empty context, not a request that never
/// happened. See `docs/notes/client-lib.md`.
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

/// Whether a refill's answer already carries this event.
///
/// # Invariant
///
/// Only the kinds `get_messages --replay true` replays: the rows a window drew, the
/// parts still streaming under them, and where the transcript ends (`last_seq`). A
/// session row, a status, a usage reading and a deletion are no part of a
/// transcript, so an answer never carries them -- and dropping one because it was
/// numbered before the answer would lose it for good.
fn restated(event: &Event) -> bool {
    match event {
        Event::Response(_)
        | Event::ToolCall(_)
        | Event::ToolAnswer(_)
        | Event::Transcript { .. } => true,
        _ => false,
    }
}

/// The session list's order: the turn that ended last comes first.
///
/// # Invariant
///
/// One key, spelled the way `Store::list_sessions` spells it: `ended_at`, or
/// `created_at` for a session that has not finished a turn, newest first, with
/// `name` breaking the tie. Both sides compute it from the same published
/// numbers, so a row that arrived as a delta lands where the server would have
/// put it. See `docs/notes/server-store.md`.
fn newest_turn_first(a: &SessionInfo, b: &SessionInfo) -> std::cmp::Ordering {
    let ended = |row: &SessionInfo| row.ended_at.unwrap_or(row.created_at);
    ended(b).cmp(&ended(a)).then_with(|| b.name.cmp(&a.name))
}

/// SPEC's `active_quota` row: the provider, then the figures it reported.
///
/// # Invariant
///
/// The label is the name SPEC gives each figure -- `month`, `week`, `5h`,
/// `credit`, `money` -- and one the provider did not report is left out rather
/// than shown as zero. See `docs/notes/client-lib.md`.
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
    /// dropping the runtime that would write it. See `docs/notes/client-lib.md`.
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
            if let Ok(ServerMessage::Event {
                event: Event::CmdResult { id, .. },
                ..
            }) = decode_server(&bytes)
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
/// The first Python that runs on this machine, by the crate's own rule.
///
/// # Invariant
///
/// One probe, shared: the Python a command line runs on and the one a skill entry
/// runs on are the same answer, or a `skill` that works here fails there.
pub fn python_program() -> anyhow::Result<std::path::PathBuf> {
    crate::skill_command::python()
}

pub fn main(args: Args) -> anyhow::Result<()> {
    if let Some(Sub::Cmd(tokens)) = &args.command
        && tokens.first().map(String::as_str) == Some("serve")
    {
        return crate::server::serve(&args, &tokens[1..]);
    }
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    runtime.block_on(async {
        match &args.command {
            None => list_commands(&args).await,
            Some(Sub::Cmd(command)) if command.first().map(String::as_str) == Some("skill") => {
                skill(&args, command).await
            }
            Some(Sub::Cmd(command)) => run(&args, command).await,
        }
    })
}

/// `skill`: `ls` and `--name` ask a server, the rest is the entry's own argv.
///
/// # Invariant
///
/// The words after `skill` belong to the entry -- `skill remote add --home x` is the
/// script's flag -- so this layer claims only what cannot be a word of it: `ls`, whose
/// answer is the session's, and `--name`/`--args`, which spell the server's command.
/// A skill that must receive `--name` itself is reachable as `skill run <name>`.
async fn skill(args: &Args, tokens: &[String]) -> anyhow::Result<()> {
    let local = || crate::skill_command::dispatch(&home_of(args), &tokens[1..]);
    let listing = tokens.get(1).map(String::as_str) == Some("ls");
    let named = tokens[2..]
        .iter()
        .any(|word| word == "--name" || word.starts_with("--name="));
    if !listing && !named {
        return local();
    }
    let Ok((mut client, mut link)) = connect(args).await else {
        return local();
    };
    let result = if client.has_command("skill") {
        match (listing, calling_session()) {
            (true, Some(session)) => match ask_table(&mut client, &mut link, &session).await {
                Ok(()) => Ok(()),
                // A shell of another server's session still has a machine to
                // list: the refusal is said, and the local walk answers.
                Err(why) => {
                    eprintln!("note: {session} is not a session this server knows ({why})");
                    local()
                }
            },
            (true, None) => {
                link.goodbye().await;
                return local();
            }
            _ => run_once(&mut client, &mut link, tokens).await,
        }
    } else {
        local()
    };
    link.goodbye().await;
    result
}

/// `skill ls` through the server: the table, printed as the same four columns.
async fn ask_table(client: &mut Client, link: &mut Link, session: &str) -> anyhow::Result<()> {
    let asked = vec!["skill".to_string(), session.to_string()];
    let (ok, message) = ask(client, link, &asked).await?;
    if !ok {
        anyhow::bail!(message);
    }
    let table: serde_json::Value = serde_json::from_str(&message)?;
    let mut lines = String::new();
    for row in table
        .get("skills")
        .and_then(|s| s.as_array())
        .into_iter()
        .flatten()
    {
        let field = |name: &str| row.get(name).and_then(|v| v.as_str()).unwrap_or_default();
        lines.push_str(&format!(
            "{}\t{}\t{}\t{}\n",
            field("name"),
            field("bucket"),
            field("description").replace(['\t', '\n'], " "),
            field("path")
        ));
    }
    print!("{lines}");
    if let Some(elsewhere) = table.get("elsewhere").and_then(|v| v.as_str()) {
        eprintln!("{elsewhere}");
    }
    Ok(())
}

/// One command asked quietly: its own answer, and nothing printed on the way.
///
/// # Invariant
///
/// Only the `cmd_result` this command was sent with ends the wait; every other event
/// is fed to the client and dropped, so a table never carries a session's news.
async fn ask(
    client: &mut Client,
    link: &mut Link,
    tokens: &[String],
) -> anyhow::Result<(bool, String)> {
    let mut printer = NullSink::default();
    let msg = match client.command(tokens.to_vec(), &mut printer) {
        Submission::Send(msg) => msg,
        Submission::Local => anyhow::bail!("{} was not sent", tokens.join(" ")),
        Submission::Rejected => anyhow::bail!(
            "{}",
            printer
                .take_error()
                .unwrap_or_else(|| "not a command".to_string())
        ),
        Submission::Offline => anyhow::bail!("not connected to the server"),
    };
    let id = match &msg {
        ClientMessage::Command { id, .. } => *id,
        other => anyhow::bail!("{other:?} is not a command"),
    };
    link.send(msg)
        .await
        .map_err(|e| anyhow!("send failed: {e}"))?;
    loop {
        let Some(bytes) = link.recv().await else {
            anyhow::bail!("server disconnected");
        };
        let event = match decode_server(&bytes)? {
            ServerMessage::Event { event, .. } => event,
            _ => continue,
        };
        client.handle_event(event.clone(), &mut printer);
        if let Event::CmdResult {
            id: rid,
            ok,
            message,
            ..
        } = &event
            && *rid == id
        {
            return Ok((*ok, message.clone()));
        }
    }
}

/// What a command row says about lines the server never answered.
///
/// # Invariant
///
/// "No answer" and not "not stored": the datagram may have arrived before the
/// link died, and the client cannot tell. It says what it knows, and the operator
/// decides whether to send the line again.
fn unanswered(lines: &[String]) -> String {
    match lines {
        [only] => format!("no answer for {only} -- the server never said it had it"),
        [_, _, ..] => format!("no answer for {} lines: {}", lines.len(), lines.join(" | ")),
        [] => String::new(),
    }
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

/// Headless: one command in, its JSON events out, then exit.
///
/// See `docs/notes/client-lib.md`.
pub async fn run(args: &Args, command: &[String]) -> anyhow::Result<()> {
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
    // Another session's stream is not this process's output; the kind says
    // which one is. See `docs/notes/client-lib.md`.
    let watched = match &msg {
        ClientMessage::Command { name, args, .. } => {
            client.session_named(name, args).map(str::to_string)
        }
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
            ServerMessage::Event { event, .. } => event,
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
/// `docs/notes/client-lib.md`.
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
/// See `docs/notes/client-lib.md`.
pub async fn dial(endpoint: Endpoint) -> Result<(Link, Greeting), DialFailure> {
    let socket = Arc::new(
        UdpSocket::bind("0.0.0.0:0")
            .await
            .map_err(|e| DialFailure::Temporary(e.into()))?,
    );
    let config = ChannelConfig::default().tune_for_peer(endpoint.addr);
    let channel = protocol::connect(socket, endpoint.addr, config)
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

/// A client for this process's home, built without touching the network.
///
/// # Invariant
///
/// No state is read: `tui.toml` is the window's, and a one-shot command names
/// every argument it means. The home is here for one reason -- where `skills/`
/// is read from -- and never for anything this process would write.
pub fn build_client(args: &Args) -> Client {
    let home = home_dir(args.conn.home.as_deref());
    Client::new(Catalog::new(), home)
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
/// who may stop it. See `docs/notes/client-lib.md`.
fn client_info() -> ClientInfo {
    ClientInfo {
        version: env!("CARGO_PKG_VERSION").to_string(),
        build: protocol::BUILD.to_string(),
        label: client_label(),
        // Only this process knows whose shell it is: `subscribe` needs it.
        session: calling_session(),
    }
}

/// What this program calls itself on the wire: its own file name and its pid.
///
/// # Invariant
///
/// The program's name, not the crate's: a window, a one-shot command and the
/// bundle's own `gongga` all dial through this one function, and a record of who
/// asked is only readable if it says which of them it was. See
/// `docs/notes/client-lib.md`.
fn client_label() -> String {
    let program = std::env::current_exe()
        .ok()
        .and_then(|path| {
            path.file_stem()
                .map(|stem| stem.to_string_lossy().into_owned())
        })
        .unwrap_or_else(|| "cli".to_string());
    format!("{program}/pid-{}", std::process::id())
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
