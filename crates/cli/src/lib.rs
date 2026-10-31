//! `gongga-cli`: the command layer.
//!
//! See `docs/notes/cli-lib.md`.

pub mod catalog;
pub mod commands;
pub mod complete;
pub mod server;

pub use crate::catalog::Catalog;
pub use crate::commands::{BlockSize, RenderModes, Sticky};
pub use crate::complete::{Candidate, Completer, Completion, SharedCompleter, token_bounds};
pub use crate::server::{ensure, spawn, starts_here};

use std::collections::HashMap;

use std::net::{SocketAddr, ToSocketAddrs};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, anyhow};
use clap::{CommandFactory, FromArgMatches, Parser};
use protocol::{
    ChannelConfig, ClientInfo, ClientMessage, Event, LogLevel, ResponseEvent, ResponsePart,
    ServerMessage, SessionInfo, SessionStatus, ToolAnswerEvent, ToolStream, decode_server,
    encode_client,
};
use tokio::net::UdpSocket;

use crate::commands::{installed_skills, parse_args, render_args};

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
    /// Connection key. Defaults to `[server] key` in ~/.gongga/config.toml.
    #[arg(long, global = true)]
    pub key: Option<String>,
}

impl Connection {
    /// Where to dial and with which key, resolved once per process.
    pub fn endpoint(&self) -> anyhow::Result<Endpoint> {
        let addr = resolve_server(self)?;
        let key = resolve_key(self).ok_or_else(|| {
            anyhow!(
                "no connection key: pass --key, set GONGGA_KEY, or start the server \
                 once to generate one"
            )
        })?;
        Ok(Endpoint { addr, key })
    }
}

#[derive(Debug, Parser)]
#[command(name = "gongga-cli")]
pub struct Args {
    #[command(flatten)]
    pub conn: Connection,
    /// Never start a server for this client; the window waits instead.
    #[arg(long, global = true)]
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
        // Every command flag takes one value, which may itself look like a flag:
        // walk pairs rather than match every token.
        let mut i = 1;
        while i < command.len() {
            let token = &command[i];
            let ordered = ["--server", "--home", "--key", "--client-only"];
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
    // `bin_name` is the word clap prints in `Usage:`, and it defaults to argv[0]:
    // without this a bundle's `gongga cli --help` answers as `gongga-cli.exe`.
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
/// `gongga-tui cli get_messages ...` is `gongga get_messages ...`: one program
/// was installed, and this is the word that says which half of it is meant. A
/// program that *is* the command layer has no such word to add -- see
/// `docs/notes/tui-main.md`.
pub const CLI: &str = "cli";

/// The name a shell types, and the name this program answers to in a home's `bin`.
///
/// The crate's own name, not a short form of it: one name for the package, the
/// program and the link, so what a reader finds in `Cargo.toml` is what they type.
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

// Where rendered output goes

/// What a rendered line is; the front-end only colours it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LineKind {
    /// The harness talking about itself: connected, offline, a status word.
    System,
    /// A command line, echoed: `/cmd` as typed, or `$ cmd --args` as it ran.
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

/// How one session's events are drawn: `set_render_session --mode`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RenderMode {
    /// A pane of its own, in the horizontal split beside the session list.
    ///
    /// # Invariant
    ///
    /// It is what a session nobody set is drawn as: a window that opens on nothing,
    /// with no line saying why, reads as a broken server.
    #[default]
    Full,
    /// One line per event, below the panes.
    Line,
    /// Nothing of this session is drawn; its lines are still held.
    None,
}

impl RenderMode {
    pub fn as_str(self) -> &'static str {
        match self {
            RenderMode::Full => "full",
            RenderMode::Line => "line",
            RenderMode::None => "none",
        }
    }

    /// The mode named, or what is wrong with the name: never a silent default.
    pub fn parse(raw: &str) -> Result<Self, String> {
        match raw.trim() {
            "full" => Ok(RenderMode::Full),
            "line" => Ok(RenderMode::Line),
            "none" => Ok(RenderMode::None),
            other => Err(format!(
                "no render mode {other:?}; it takes {}",
                protocol::RENDER_MODES.join(", ")
            )),
        }
    }
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
/// One decision read four ways: a session is in `none`, in `full` or in `line`,
/// never in two -- SPEC gives each render mode one area of the window, and a name
/// drawn twice would be a second place saying the same thing. `listed` is the
/// announcement itself, every session with its status and its numbers, so a
/// front-end can read a name it draws anywhere.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Layout {
    /// Every session the server knows, in its order.
    pub listed: Vec<Listed>,
    /// Sessions drawn as a row of the session list -- SPEC's `render mode = none`.
    pub none: Vec<String>,
    /// Sessions drawn as a pane each, in the list's order.
    pub full: Vec<String>,
    /// Sessions drawn as one line per event, in the list's order.
    pub line: Vec<String>,
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
    /// How the window lays itself out. A front-end that ignores it shows one list.
    fn layout(&mut self, _layout: &Layout) {}
    /// Empties the window `window` names: the session that wrote it is deleted.
    fn forget(&mut self, _window: &str) {}
    /// The session the sticky `session` names: what the keys scroll and what a
    /// click sets. See `docs/notes/cli-lib.md`.
    fn sticky_session(&mut self, _name: Option<&str>) {}
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
    /// How each session's events are drawn.
    render: RenderModes,
    /// Which window each accepted command's result belongs to, from its echo.
    results: HashMap<u64, String>,
    /// A `set_session` waiting for its acknowledgement, so a refused one writes
    /// nothing. See `docs/notes/cli-lib.md`.
    follows: Vec<(u64, String)>,
    sessions: Vec<SessionInfo>,
    model: String,
    next_id: u64,
    /// Whether a link is open right now; while false, all server knowledge is
    /// stale.
    connected: bool,
    /// Failed dials since the link went down, so silence is legible.
    attempts: u32,
    /// Set by `connect_server`; the front-end reads it and dials again.
    server_changed: bool,
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

    /// Whether an event's lines are drawn: by its own session's mode, or -- for an
    /// event that names no session -- at all.
    fn drawn(&self, event: &Event) -> bool {
        let session = match event {
            Event::Response(e) => Some(e.session.as_str()),
            Event::ToolAnswer(e) => Some(e.session.as_str()),
            Event::Status { session, .. } => Some(session.as_str()),
            _ => None,
        };
        session.is_none_or(|name| self.render.mode_of(name) != RenderMode::None)
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
    /// whichever area the session's mode puts it -- the list, a pane's title, or a
    /// line group's row. Two places saying it is how `[main] [main] ...` happened.
    /// See `docs/notes/cli-lib.md`.
    fn tag(&self, _session: &str, label: &str) -> String {
        label.to_string()
    }

    /// How the window lays itself out: SPEC's UI section, as three lists.
    ///
    /// # Invariant
    ///
    /// Every session the server knows is listed whatever its mode -- the list is
    /// how a mode gets changed -- and each one is in exactly one of the two
    /// drawing lists, or in neither when it is `none`.
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
            // SPEC's list is the `none` mode, so it holds what no other area draws.
            none: self
                .sessions
                .iter()
                .filter(|s| self.render.mode_of(&s.name) == RenderMode::None)
                .map(|s| s.name.clone())
                .collect(),
            // A session nobody set is `full`, so the lists read the sessions,
            // not the file's entries.
            full: self
                .sessions
                .iter()
                .filter(|s| self.render.mode_of(&s.name) == RenderMode::Full)
                .map(|s| s.name.clone())
                .collect(),
            line: self
                .sessions
                .iter()
                .filter(|s| self.render.mode_of(&s.name) == RenderMode::Line)
                .map(|s| s.name.clone())
                .collect(),
            command: self.command_window(),
            quota: self.quota.clone(),
        }
    }

    fn new(catalog: Catalog, sticky: Sticky) -> Self {
        let block_size = BlockSize::load(sticky.home());
        let render = RenderModes::load(sticky.home());
        catalog.set_sticky_names(&sticky.names());
        Self {
            catalog,
            sticky,
            block_size,
            render,
            results: HashMap::new(),
            follows: Vec::new(),
            sessions: Vec::new(),
            model: "-".to_string(),
            next_id: Self::seed_command_id(),
            connected: false,
            attempts: 0,
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
                ..
            } => {
                self.model = server.model.clone();
                // A redial is a resync, so a session deleted while this client was
                // away leaves the list here too.
                self.set_sessions(sessions, sink);
                self.catalog.set_commands(commands);
                self.sync_sticky();
                // A greeting *is* the recovery: it carries the catalog and
                // sessions, so a redial comes up like a fresh client.
                self.connected = true;
                self.attempts = 0;
                // The facts are the announcement: "offline — reconnecting"
                // becomes the model and the session count. A warning about this
                // machine belongs beside them, not in place of them.
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
        }
    }

    fn handle_event(&mut self, event: Event, sink: &mut dyn Sink) {
        // A render mode hides lines, not state: a `Status` still updates the list.
        let shown = self.drawn(&event);
        match event {
            Event::Cmd { id, name, args } => {
                let window = self.command_line(args.get("session").map(String::as_str));
                // The result carries only the id, so say now where its answer goes.
                self.results.insert(id, window.clone());
                if shown {
                    let line = format!("$ {name}{}", render_args(&args));
                    sink.line(&window, LineKind::Command, &line);
                }
            }
            Event::CmdResult {
                id,
                message,
                ok,
                data,
                ..
            } => {
                self.settle_follow(id, ok, sink);
                if shown {
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
                        sink.line(&window, kind, &message);
                    }
                }
            }
            Event::Response(e) => {
                if shown {
                    self.handle_response(e, sink);
                }
            }
            Event::ToolAnswer(e) => {
                if shown {
                    self.handle_tool_answer(e, sink);
                }
            }
            Event::Sessions { sessions } => {
                self.set_sessions(sessions, sink);
                self.refresh(sink);
            }
            Event::Status {
                session,
                status,
                detail,
            } => {
                if let Some(s) = self.sessions.iter_mut().find(|s| s.name == session) {
                    s.status = status;
                }
                if shown {
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
            Event::Log { level, message } => {
                if shown {
                    let kind = match level {
                        LogLevel::Debug | LogLevel::Info => LineKind::System,
                        LogLevel::Warn | LogLevel::Error => LineKind::Error,
                    };
                    sink.note(kind, &message);
                }
            }
        }
    }

    fn handle_response(&self, e: ResponseEvent, sink: &mut dyn Sink) {
        // One part is one block: the window already keeps the sessions apart.
        let key = format!("r:{}:{}:{:?}", e.session, e.turn, e.part);
        let (kind, label) = match e.part {
            // No label: the kind's colour (dim italic) already says this is the
            // model thinking, and the label was the word twice.
            ResponsePart::Reasoning => (LineKind::Reasoning, ""),
            // No label: the colour says a tool ran, and the text is the call.
            ResponsePart::ToolCall => (LineKind::ToolCall, ""),
            // Kept: it shares the tool call's colour, so the word is the only
            // thing saying who ran it.
            ResponsePart::Activity => (LineKind::ToolCall, "provider: "),
            ResponsePart::Output => (LineKind::Assistant, ""),
        };
        // The tag says which part of the answer this is; the window says who speaks.
        let prefix = self.tag(&e.session, label);
        let window = e.session.clone();
        sink.stream(&window, &key, kind, &prefix, &e.text, e.done);
    }

    fn handle_tool_answer(&self, e: ToolAnswerEvent, sink: &mut dyn Sink) {
        let key = format!("t:{}:{:?}", e.call_id, e.stream);
        let kind = match e.stream {
            ToolStream::Stdout => LineKind::ToolOutput,
            ToolStream::Stderr => LineKind::ToolError,
        };
        // A tool's output is its own block, in the window of the session that ran it.
        let prefix = self.tag(&e.session, "");
        let window = e.session.clone();
        sink.stream(&window, &key, kind, &prefix, &e.text, e.done);
    }

    fn session_names(&self) -> Vec<String> {
        self.sessions.iter().map(|s| s.name.clone()).collect()
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
        self.sessions = sessions;
        self.catalog.set_sessions(self.session_names());
    }

    /// Keeps the completer's state-backed sources in step: the sticky names, and
    /// the skills this home can open.
    pub fn sync_sticky(&self) {
        self.catalog.set_sticky_names(&self.sticky.names());
        self.catalog
            .set_skill_names(&installed_skills(self.sticky.home()));
    }

    /// The status line: server facts only, never a "current session".
    /// See `docs/notes/cli-lib.md`.
    pub fn refresh(&self, sink: &mut dyn Sink) {
        // Told on every refresh, so a front-end attached late still learns it.
        sink.block_size(self.block_size.0);
        sink.layout(&self.layout());
        sink.sticky_session(self.sticky.get("session"));
        let mut facts = if self.connected {
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

    /// Sends one line of prose as one argument of `send_message`.
    pub fn message(&mut self, text: &str, sink: &mut dyn Sink) -> Submission {
        let tokens = vec![
            "send_message".to_string(),
            "--message".to_string(),
            text.to_string(),
        ];
        self.submit_tokens(tokens, sink, Echo::Message(text))
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
            sink.line(
                &self.command_window(),
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
                    sink.line(
                        &self.command_window(),
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
            sink.line(
                &self.command_window(),
                LineKind::Error,
                &format!("unknown command {head:?}; {why}"),
            );
            return Submission::Rejected;
        };

        // One validation, by the published spec: `target` picks the path.
        let mut args = match parse_args(&tokens[1..]) {
            Ok(a) => a,
            Err(e) => {
                sink.line(&self.command_window(), LineKind::Error, &e.to_string());
                return Submission::Rejected;
            }
        };
        // A flag the command does not take is a typo, not decoration; checked
        // before `fill`, so only what was typed is judged.
        let unknown: Vec<String> = args
            .keys()
            .filter(|k| !spec.args.iter().any(|a| a.name == **k))
            .map(|k| format!("--{k}"))
            .collect();
        if !unknown.is_empty() {
            let known: Vec<String> = spec.args.iter().map(|a| format!("--{}", a.name)).collect();
            sink.line(
                &self.command_window(),
                LineKind::Error,
                &format!(
                    "{head} has no argument {}; it takes {}",
                    unknown.join(", "),
                    known.join(", ")
                ),
            );
            return Submission::Rejected;
        }
        self.sticky.fill(Some(&spec), &mut args);
        let missing: Vec<&str> = spec
            .args
            .iter()
            .filter(|a| a.required && !args.contains_key(&a.name))
            .map(|a| a.name.as_str())
            .collect();
        if !missing.is_empty() {
            self.show(
                sink,
                &echo,
                &tokens,
                args.get("session").map(String::as_str),
            );
            sink.line(
                &self.command_window(),
                LineKind::Error,
                &format!(
                    "{head} is missing required argument(s): {}",
                    missing.join(", ")
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
        if head == "set_session" {
            if let Some(session) = args.get("session") {
                self.follows.push((id, session.clone()));
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
                sink.line(&self.command_window(), LineKind::Error, &format!("{e}"));
                return;
            }
        };
        match commands::Remembered::save(self.sticky.home(), addr) {
            Ok(()) => {
                sink.line(
                    &self.command_window(),
                    LineKind::Result,
                    &format!("server is {addr}; reconnecting"),
                );
                self.server_changed = true;
            }
            Err(e) => sink.line(
                &self.command_window(),
                LineKind::Error,
                &format!("could not save: {e}"),
            ),
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
        let (_, session) = self.follows.remove(at);
        if !ok {
            return;
        }
        if let Err(e) = self.sticky.set("session", &session) {
            sink.line(
                &self.command_window(),
                LineKind::Error,
                &format!("could not save: {e}"),
            );
        }
        self.after_sticky_change(sink);
    }

    /// Runs one client command, whose args the spec already validated.
    fn run_client_command(&mut self, name: &str, args: &protocol::Args, sink: &mut dyn Sink) {
        let arg_name = args.get("name").cloned().unwrap_or_default();
        match name {
            "connect_server" => self.remember_server(args, sink),
            // The process is the server: a window cannot draw it, so say
            // where to type it.
            "serve" => sink.line(
                &self.command_window(),
                LineKind::Error,
                &format!(
                    "serve takes over the process; run it in a shell: `{} serve --port <port>`",
                    typed_name()
                ),
            ),
            "set_render" => self.run_render(args, sink),
            "set_render_session" => self.run_render_session(args, sink),
            "set_sticky" => {
                let Some(value) = args.get("value").cloned() else {
                    sink.line(
                        &self.command_window(),
                        LineKind::Error,
                        "set_sticky requires --value",
                    );
                    return;
                };
                match self.sticky.set(&arg_name, &value) {
                    Ok(()) => {
                        sink.line(
                            &self.command_window(),
                            LineKind::Result,
                            &format!("sticky {arg_name} = {value}"),
                        );
                        self.after_sticky_change(sink);
                    }
                    Err(e) => sink.line(
                        &self.command_window(),
                        LineKind::Error,
                        &format!("could not save: {e}"),
                    ),
                }
            }
            "del_sticky" => match self.sticky.del(&arg_name) {
                Ok(true) => {
                    sink.line(
                        &self.command_window(),
                        LineKind::Result,
                        &format!("sticky {arg_name} cleared"),
                    );
                    self.after_sticky_change(sink);
                }
                Ok(false) => sink.line(
                    &self.command_window(),
                    LineKind::Error,
                    &format!("no sticky argument named {arg_name:?}"),
                ),
                Err(e) => sink.line(
                    &self.command_window(),
                    LineKind::Error,
                    &format!("could not save: {e}"),
                ),
            },
            // Published as a client command this build cannot run: say so.
            other => sink.line(
                &self.command_window(),
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
        let Some(raw) = args.get("block_size") else {
            let report = format!("render block_size = {}", self.block_size.0);
            sink.line(&self.command_window(), LineKind::Result, &report);
            return;
        };
        let parsed = match BlockSize::parse(raw) {
            Ok(parsed) => parsed,
            Err(e) => {
                sink.line(&self.command_window(), LineKind::Error, &e);
                return;
            }
        };
        match parsed.save(self.sticky.home()) {
            Ok(()) => {
                self.block_size = parsed;
                sink.line(
                    &self.command_window(),
                    LineKind::Result,
                    &format!("render block_size = {}", parsed.0),
                );
                self.after_sticky_change(sink);
            }
            Err(e) => sink.line(
                &self.command_window(),
                LineKind::Error,
                &format!("could not save: {e}"),
            ),
        }
    }

    /// Runs `set_render_session`: how one session's events are drawn.
    ///
    /// # Invariant
    ///
    /// It changes a view and never the state: the blocks a session already said
    /// are still held, so `--mode full` after `--mode none` brings them back.
    fn run_render_session(&mut self, args: &protocol::Args, sink: &mut dyn Sink) {
        let name = args.get("session").cloned().unwrap_or_default();
        let Some(raw) = args.get("mode") else {
            let mode = self.render.mode_of(&name);
            sink.line(
                &self.command_window(),
                LineKind::Result,
                &format!("render {name} = {}", mode.as_str()),
            );
            return;
        };
        let mode = match RenderMode::parse(raw) {
            Ok(mode) => mode,
            Err(e) => {
                sink.line(&self.command_window(), LineKind::Error, &e);
                return;
            }
        };
        match self.render.set(&name, mode) {
            Ok(()) => {
                sink.line(
                    &self.command_window(),
                    LineKind::Result,
                    &format!("render {name} = {}", mode.as_str()),
                );
                self.after_sticky_change(sink);
            }
            Err(e) => sink.line(
                &self.command_window(),
                LineKind::Error,
                &format!("could not save: {e}"),
            ),
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
    if let Some(provider) = &session.provider {
        parts.push(format!("provider {provider}"));
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
    // A row that only names its provider and a complaint is a failure to read;
    // one with figures carries a note beside them and stays a fact.
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

    /// Best-effort goodbye, so the server can drop this connection now.
    pub async fn goodbye(&self) {
        let _ = self.send(ClientMessage::Bye).await;
    }

    /// Stops the server as SPEC's `cancel` does: a kill would lose the work it
    /// stores for the next start.
    pub async fn cancel(&self) {
        let _ = self
            .send(ClientMessage::Command {
                id: 0,
                name: "cancel".to_string(),
                args: Default::default(),
            })
            .await;
    }
}

/// Runs one command line, with the runtime each command needs.
///
/// `forward` is the subcommand a shell has to type to reach the command layer
/// through this program -- `Some(CLI)` from the window, `None` from `gongga`
/// itself. It only decides what the server installs for its own shells.
///
/// # Invariant
///
/// `serve` is decided before a runtime exists: it leaves nothing of the client
/// behind, and the server builds the runtime it wants to be stopped in.
pub fn main(args: Args) -> anyhow::Result<()> {
    if let Some(Sub::Cmd(tokens)) = &args.command {
        if tokens.first().map(String::as_str) == Some("serve") {
            return crate::server::serve(&args, &tokens[1..]);
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
    let parsed = parse_args(&command[1..])?;
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
        Submission::Send(msg) => msg,
    };
    let id = match &msg {
        ClientMessage::Command { id, .. } => *id,
        other => anyhow::bail!("{command:?} produced {other:?}, expected a command"),
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
        let ServerMessage::Event(event) = decode_server(&bytes)? else {
            continue;
        };
        // Feed the client too: a one-shot `set_session` still leaves a sticky
        // `session` behind.
        client.handle_event(event.clone(), &mut printer);
        // The event *is* the output; JSON lines survive `jq` and a pipe.
        println!("{}", serde_json::to_string(&event)?);
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
        "{} command(s); run one with `gongga <command> --<arg> <value>`.",
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
            signature(&command.args),
        );
    }
    Ok(())
}

/// `--a (required), --b (optional)`, or `nothing` when a command takes none.
fn signature(args: &[protocol::ArgSpec]) -> String {
    if args.is_empty() {
        return "nothing".to_string();
    }
    args.iter()
        .map(|a| {
            let needed = if a.required { "required" } else { "optional" };
            format!("--{} ({needed})", a.name)
        })
        .collect::<Vec<_>>()
        .join(", ")
}

// Connection

/// A resolved address and key, fixed for the life of the process.
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
    let key = resolve_key(args).ok_or_else(|| {
        anyhow!("no connection key: pass --key, set GONGGA_KEY, or start the server once to generate one")
    })?;
    Ok(Endpoint { addr, key })
}

/// Why a dial did not produce a link.
pub enum DialFailure {
    /// Asking again cannot help: wrong key, malformed address.
    Fatal(anyhow::Error),
    /// The address refused the datagram: nothing is listening there. This is the
    /// one failure starting a server is an answer to.
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
                "{}; is a server running? Start one with `{} serve --port <port>`",
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
fn client_info() -> ClientInfo {
    ClientInfo {
        version: env!("CARGO_PKG_VERSION").to_string(),
        build: protocol::BUILD.to_string(),
        label: format!("cli/pid-{}", std::process::id()),
    }
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

/// `--key`, then `GONGGA_KEY`, then what the home itself carries.
fn resolve_key(args: &Connection) -> Option<String> {
    resolve_key_with(args, &process_env)
}

fn resolve_key_with(args: &Connection, env: &Env) -> Option<String> {
    if let Some(k) = &args.key {
        return Some(k.clone());
    }
    env("GONGGA_KEY").or_else(|| key_from_home(&home_dir(args.home.as_deref())))
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

/// What a home itself says its key is: `server.key` first, because a server that
/// minted one wrote it there, then a `[server] key` a home written before that
/// still has.
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

#[cfg(test)]
mod tests {
    use super::*;
    use protocol::encode_server;

    fn home_with(text: &str) -> tempfile::TempDir {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(d.path().join("config.toml"), text).unwrap();
        d
    }

    /// A command as argv, the way the one-shot path receives it.
    fn argv(line: &str) -> Vec<String> {
        shell_words::split(line).expect("test argv parses")
    }

    fn parsed_command(command: Vec<String>) -> Args {
        Args {
            conn: Connection {
                server: None,
                home: None,
                key: None,
            },
            client_only: false,
            command: Some(Sub::Cmd(command)),
        }
    }

    #[test]
    fn flag_order_check_skips_values_that_look_like_global_flags() {
        for value in ["--key", "--home", "--server"] {
            let args = parsed_command(argv(&format!(
                "send_message --message {value} --session=--key"
            )));
            assert!(args.check_flag_order().is_ok(), "value was {value}");
        }
    }

    /// `serve --home` reads as one sentence.
    #[test]
    fn only_serve_takes_a_home_after_its_name() {
        for command in [
            "serve --port 9700 --home /tmp/other",
            "serve --port 9700 --home=/tmp/other",
        ] {
            let args = parsed_command(argv(command));
            assert!(args.check_flag_order().is_ok(), "rejected {command}");
        }
        // The other three are still where clap can see them or nowhere.
        for command in [
            "serve --port 9700 --key secret",
            "serve --port 9700 --server 127.0.0.1:9701",
        ] {
            let args = parsed_command(argv(command));
            assert!(args.check_flag_order().is_err(), "accepted {command}");
        }
    }

    #[test]
    fn flag_order_check_still_rejects_misplaced_global_flags() {
        for command in [
            "send_message --message hi --key secret",
            "send_message --home=/tmp/other --message hi",
            "send_message --server 127.0.0.1:9701 --message hi",
        ] {
            let args = parsed_command(argv(command));
            assert!(args.check_flag_order().is_err(), "accepted {command}");
        }
    }

    /// A quoted argument containing a space has to stay one argument.
    #[test]
    fn an_argument_that_contains_a_space_stays_one_argument() {
        let d = tempfile::tempdir().unwrap();
        let mut c = client_with(send_message_catalog(), Sticky::load(d.path()), vec!["main"]);
        let mut s = VecSink(Vec::new());
        match c.command(argv("send_message --message 'two words'"), &mut s) {
            Submission::Send(ClientMessage::Command { args, .. }) => {
                assert_eq!(args["message"], "two words");
            }
            _ => panic!("expected a message to send"),
        }
    }

    /// `/` is the interactive line's syntax; the rejection spells out the line
    /// meant. See `docs/notes/cli-lib.md`.
    #[test]
    fn a_leading_slash_is_refused_with_the_command_line_it_meant() {
        let d = tempfile::tempdir().unwrap();
        let mut c = client_with(send_message_catalog(), Sticky::load(d.path()), vec!["main"]);
        let mut s = VecSink(Vec::new());
        assert!(matches!(
            c.command(argv("/send_message --message hi"), &mut s),
            Submission::Rejected
        ));
        let said = s.0.join("\n");
        assert!(said.contains("interactive"), "{said}");
        assert!(said.contains("gongga send_message --message hi"), "{said}");
    }

    /// The same mistake after Git Bash turned `/get_messages` into a path.
    #[test]
    fn a_shell_rewritten_slash_is_recognised() {
        let d = tempfile::tempdir().unwrap();
        let mut c = client_with(send_message_catalog(), Sticky::load(d.path()), vec!["main"]);
        for arg in [
            r"C:\Program Files\Git\send_message",
            r"C:\Program Files\Git\send_message --message hi",
            "/usr/bin/send_message --message hi",
        ] {
            let mut s = VecSink(Vec::new());
            assert!(
                matches!(
                    c.command(vec![arg.to_string()], &mut s),
                    Submission::Rejected
                ),
                "{arg} was accepted"
            );
            let said = s.0.join("\n");
            assert!(said.contains("gongga send_message"), "{arg}: {said}");
            assert!(said.contains("interactive"), "{arg}: {said}");
        }
    }

    /// The command layer must not depend on a front-end: that is the split this
    /// crate exists for.
    #[test]
    fn the_command_layer_does_not_depend_on_a_front_end() {
        let manifest = include_str!("../Cargo.toml");
        let deps = manifest
            .split("[dependencies]")
            .nth(1)
            .and_then(|rest| rest.split("\n[").next())
            .unwrap_or_default();
        for forbidden in ["tui =", "ratatui", "crossterm", "futures-util"] {
            assert!(
                !deps.contains(forbidden),
                "the command layer must not depend on {forbidden}:\n{deps}"
            );
        }
        // Its one program is the command line; the terminal is another crate.
        assert!(
            manifest
                .lines()
                .any(|l| l.trim() == "name = \"gongga-cli\""),
            "the command layer's own binary is the command line:\n{manifest}"
        );
    }

    /// A shell with nothing set, so every precedence test states its own.
    fn no_env(_: &str) -> Option<String> {
        None
    }

    #[test]
    fn explicit_flag_wins() {
        let d = home_with("[server]\nbind = \"127.0.0.1:9999\"\nkey = \"abc\"\n");
        let args = Connection {
            server: Some("10.0.0.5:1234".into()),
            home: Some(d.path().to_path_buf()),
            key: None,
        };
        let env = |k: &str| match k {
            "GONGGA_SERVER" => Some("127.0.0.1:8888".to_string()),
            _ => None,
        };
        assert_eq!(
            resolve_server_with(&args, &env).unwrap().to_string(),
            "10.0.0.5:1234"
        );
    }

    #[test]
    fn config_file_is_the_last_resort() {
        let d = home_with("[server]\nbind = \"127.0.0.1:9999\"\nkey = \"abc\"\n");
        let args = Connection {
            server: None,
            home: Some(d.path().to_path_buf()),
            key: None,
        };
        assert_eq!(
            resolve_server_with(&args, &no_env).unwrap().to_string(),
            "127.0.0.1:9999"
        );
        assert_eq!(resolve_key_with(&args, &no_env).as_deref(), Some("abc"));
    }

    /// The harness sets both, so the shell beats the config file.
    #[test]
    fn the_environment_beats_the_config_file() {
        let d = home_with("[server]\nbind = \"127.0.0.1:9999\"\nkey = \"abc\"\n");
        let args = Connection {
            server: None,
            home: Some(d.path().to_path_buf()),
            key: None,
        };
        let env = |k: &str| match k {
            "GONGGA_SERVER" => Some("127.0.0.1:9700".to_string()),
            "GONGGA_KEY" => Some("from-shell".to_string()),
            _ => None,
        };
        assert_eq!(
            resolve_server_with(&args, &env).unwrap().to_string(),
            "127.0.0.1:9700"
        );
        assert_eq!(resolve_key_with(&args, &env).as_deref(), Some("from-shell"));
    }

    #[test]
    fn key_flag_beats_config() {
        let d = home_with("[server]\nkey = \"abc\"\n");
        let args = Connection {
            server: None,
            home: Some(d.path().to_path_buf()),
            key: Some("zzz".into()),
        };
        assert_eq!(resolve_key_with(&args, &no_env).as_deref(), Some("zzz"));
    }

    #[test]
    fn missing_key_yields_nothing() {
        let d = home_with("[server]\nbind = \"127.0.0.1:9999\"\n");
        let args = Connection {
            server: None,
            home: Some(d.path().to_path_buf()),
            key: None,
        };
        assert!(resolve_key_with(&args, &no_env).is_none());
    }

    /// Drives a command without a terminal or a socket, marked connected.
    fn client_with(catalog: Catalog, sticky: Sticky, sessions: Vec<&str>) -> Client {
        let mut c = Client::new(catalog, sticky);
        c.connected = true;
        c.sessions = sessions.into_iter().map(session).collect();
        c
    }

    /// One session as the server announces it, with nothing measured in it yet.
    fn session(name: &str) -> SessionInfo {
        SessionInfo {
            name: name.into(),
            status: SessionStatus::Idle,
            created_at: 0,
            updated_at: 0,
            messages: 0,
            cwd: String::new(),
            usage: protocol::Usage::default(),
            provider: None,
            thinking_effort: None,
            context_limit: None,
        }
    }

    /// A greeting as a redial brings it: the catalog and the whole session list.
    fn greeting(sessions: Vec<&str>) -> ServerMessage {
        ServerMessage::Hello {
            protocol: protocol::PROTOCOL_VERSION,
            server: protocol::ServerInfo {
                version: "test".into(),
                build: protocol::BUILD.into(),
                pid: 1,
                started_at: 0,
                home: String::new(),
                cwd: String::new(),
                model: "m".into(),
            },
            commands: Vec::new(),
            sessions: sessions.into_iter().map(session).collect(),
        }
    }

    /// A name leaving the announcement is a deletion: its window leaves with it.
    ///
    /// # Invariant
    ///
    /// The server hands a freed name out again, so a session made under it must not
    /// open on the deleted one's lines -- on a streamed list, and on the greeting a
    /// redial brings.
    #[test]
    fn a_deleted_session_forgets_its_window() {
        let d = tempfile::tempdir().unwrap();
        let mut c = client_with(Catalog::new(), Sticky::load(d.path()), vec!["main", "peer"]);
        let mut s = StreamSink::default();
        c.handle_event(
            Event::Sessions {
                sessions: vec![session("main")],
            },
            &mut s,
        );
        assert_eq!(s.forgotten, vec!["peer"]);

        // Nothing left the list, so nothing was forgotten.
        c.handle_event(
            Event::Sessions {
                sessions: vec![session("main")],
            },
            &mut s,
        );
        assert_eq!(
            s.forgotten,
            vec!["peer"],
            "an unchanged list forgot a window"
        );

        // A redial is the same announcement: a session deleted while this client
        // was away is gone here too.
        let mut c = client_with(Catalog::new(), Sticky::load(d.path()), vec!["main", "peer"]);
        c.handle(greeting(vec!["main"]), &mut s);
        assert_eq!(s.forgotten, vec!["peer", "peer"]);
    }

    /// The session an answer was addressed to may be the one the command deleted.
    ///
    /// # Invariant
    ///
    /// `del_session` publishes the shortened list before the command's result, so
    /// the answer has to move to the command line: its own window is gone, and a
    /// line left there would open the next session under that name.
    #[test]
    fn an_answer_to_a_deleted_session_goes_to_the_command_line() {
        let d = tempfile::tempdir().unwrap();
        let mut c = client_with(Catalog::new(), Sticky::load(d.path()), vec!["main", "peer"]);
        let mut s = StreamSink::default();
        let mut args = protocol::Args::new();
        args.insert("session".into(), "peer".into());
        c.handle_event(
            Event::Cmd {
                id: 7,
                name: "del_session".into(),
                args,
            },
            &mut s,
        );
        c.handle_event(
            Event::Sessions {
                sessions: vec![session("main")],
            },
            &mut s,
        );
        c.handle_event(
            Event::CmdResult {
                id: 7,
                ok: true,
                message: "deleted session peer".into(),
                data: None,
            },
            &mut s,
        );
        assert_eq!(s.windows, vec!["peer", COMMAND_WINDOW]);
        assert_eq!(
            s.lines,
            vec!["$ del_session --session peer", "deleted session peer"]
        );
    }

    /// A catalog whose one command is `set_session`: the server's, not ours.
    fn set_session_catalog() -> Catalog {
        let cat = Catalog::new();
        cat.set_commands(vec![protocol::CommandSpec {
            target: protocol::CommandTarget::Server,
            name: "set_session".into(),
            description: "Create or configure a session.".into(),
            args: vec![protocol::ArgSpec {
                name: "session".into(),
                description: "Target.".into(),
                required: true,
                value_hint: None,
                completions: protocol::CompletionSource::Sessions,
            }],
        }]);
        cat
    }

    /// The acknowledgement of the command just sent, so a test can answer one.
    fn ack(c: &mut Client, ok: bool, sink: &mut dyn Sink) {
        let id = c.next_id - 1;
        c.handle_event(
            Event::CmdResult {
                id,
                ok,
                message: String::new(),
                data: None,
            },
            sink,
        );
    }

    /// Draws every session the client knows in one mode: what the old split meant
    /// for the one session a test creates.
    fn draw_all(c: &mut Client, mode: RenderMode) {
        for name in c.session_names() {
            c.render.set(&name, mode).unwrap();
        }
    }

    fn published_catalog() -> Catalog {
        let cat = Catalog::new();
        cat.set_commands(protocol::client_commands());
        cat
    }

    /// The smallest catalog that can take a message: one command, one required
    /// argument, one sticky one.
    fn send_message_catalog() -> Catalog {
        let cat = Catalog::new();
        cat.set_commands(vec![protocol::CommandSpec {
            target: protocol::CommandTarget::Server,
            name: "send_message".into(),
            description: "s".into(),
            args: vec![
                protocol::ArgSpec {
                    name: "message".into(),
                    description: "s".into(),
                    required: true,
                    value_hint: None,
                    completions: protocol::CompletionSource::None,
                },
                protocol::ArgSpec {
                    name: "session".into(),
                    description: "s".into(),
                    required: false,
                    value_hint: None,
                    completions: protocol::CompletionSource::None,
                },
            ],
        }]);
        cat
    }

    struct VecSink(Vec<String>);

    impl Sink for VecSink {
        fn note(&mut self, kind: LineKind, text: &str) {
            // A note is not a window line; a test that wants lines does not want
            // these in the same list. See `docs/notes/cli-lib.md`.
            if matches!(kind, LineKind::Error) {
                self.0.push(format!("note: {text}"));
            }
        }
        fn line(&mut self, _window: &str, _kind: LineKind, text: &str) {
            self.0.push(text.to_string());
        }
        fn stream(
            &mut self,
            _window: &str,
            _k: &str,
            _kind: LineKind,
            _p: &str,
            text: &str,
            _d: bool,
        ) {
            self.0.push(text.to_string());
        }
        fn status(&mut self, _t: &str) {}
    }

    /// Keeps the window and the prefix, which `VecSink` drops: routing is what the
    /// event tests are about.
    #[derive(Default)]
    struct StreamSink {
        windows: Vec<String>,
        keys: Vec<String>,
        lines: Vec<String>,
        /// The harness's own notes, which name no window at all.
        notes: Vec<String>,
        /// The last announcement, which is what a front-end lays out from.
        announced: Option<Layout>,
        /// Windows a session's deletion emptied.
        forgotten: Vec<String>,
    }

    impl Sink for StreamSink {
        fn note(&mut self, _kind: LineKind, text: &str) {
            self.notes.push(text.to_string());
        }
        fn line(&mut self, window: &str, _kind: LineKind, text: &str) {
            self.windows.push(window.to_string());
            self.lines.push(text.to_string());
        }
        fn stream(
            &mut self,
            window: &str,
            key: &str,
            _kind: LineKind,
            prefix: &str,
            text: &str,
            _done: bool,
        ) {
            self.windows.push(window.to_string());
            self.keys.push(key.to_string());
            self.lines.push(format!("{prefix}{text}"));
        }
        fn status(&mut self, _t: &str) {}
        fn layout(&mut self, layout: &Layout) {
            self.announced = Some(layout.clone());
        }
        fn forget(&mut self, window: &str) {
            self.forgotten.push(window.to_string());
        }
    }

    /// One response event, as the server sends it.
    fn response(session: &str, part: ResponsePart, text: &str) -> ResponseEvent {
        ResponseEvent {
            session: session.into(),
            turn: 1,
            part,
            text: text.into(),
            done: false,
        }
    }

    /// A line never names its session: the window draws the name, in every mode.
    ///
    /// # Invariant
    ///
    /// The text is the same whatever the mode is. Two places saying the name is
    /// how a row comes to read `[peer] [peer] hi there`.
    #[test]
    fn a_response_line_never_carries_its_session_name() {
        let d = tempfile::tempdir().unwrap();
        let mut c = client_with(Catalog::new(), Sticky::load(d.path()), vec!["peer"]);
        let mut s = StreamSink::default();
        c.handle_response(response("peer", ResponsePart::Reasoning, "hmm"), &mut s);
        c.handle_response(response("peer", ResponsePart::ToolCall, "bash"), &mut s);
        c.handle_response(response("peer", ResponsePart::Output, "hi there"), &mut s);
        assert_eq!(
            s.lines,
            vec!["hmm", "bash", "hi there"],
            "no part needs a label: the colour says which part it is"
        );

        // Drawn as one line among others: the row's own name says whose it is.
        s.lines.clear();
        c.render.set("peer", RenderMode::Line).unwrap();
        c.handle_response(response("peer", ResponsePart::Output, "hi there"), &mut s);
        assert_eq!(s.lines, vec!["hi there"]);
    }

    /// The key stays what it was: the session there is for routing, not display.
    #[test]
    fn a_response_key_is_unchanged_by_the_tag() {
        let d = tempfile::tempdir().unwrap();
        let c = client_with(Catalog::new(), Sticky::load(d.path()), vec!["peer"]);
        let mut s = StreamSink::default();
        c.handle_response(response("peer", ResponsePart::Output, "x"), &mut s);
        assert_eq!(s.keys, vec!["r:peer:1:Output"]);
    }

    /// stdout and stderr stay separate streams, and neither carries the name.
    #[test]
    fn a_tool_answer_line_names_its_session_where_it_must() {
        let d = tempfile::tempdir().unwrap();
        let mut c = client_with(Catalog::new(), Sticky::load(d.path()), vec!["peer"]);
        c.render.set("peer", RenderMode::Line).unwrap();
        let mut s = StreamSink::default();
        for (stream, text) in [(ToolStream::Stdout, "out"), (ToolStream::Stderr, "err")] {
            c.handle_tool_answer(
                ToolAnswerEvent {
                    session: "peer".into(),
                    call_id: "c1".into(),
                    tool: "bash".into(),
                    stream,
                    text: text.into(),
                    done: false,
                },
                &mut s,
            );
        }
        assert_eq!(s.lines, vec!["out", "err"]);
        assert_eq!(s.keys, vec!["t:c1:Stdout", "t:c1:Stderr"]);

        // In a pane of its own, the output needs no name over it.
        c.render.set("peer", RenderMode::Full).unwrap();
        s.lines.clear();
        c.handle_tool_answer(
            ToolAnswerEvent {
                session: "peer".into(),
                call_id: "c1".into(),
                tool: "bash".into(),
                stream: ToolStream::Stdout,
                text: "out".into(),
                done: false,
            },
            &mut s,
        );
        assert_eq!(s.lines, vec!["out"]);
    }

    /// A log already names itself; a command belongs to the operator.
    #[test]
    fn logs_and_commands_are_not_tagged() {
        let d = tempfile::tempdir().unwrap();
        let mut c = client_with(Catalog::new(), Sticky::load(d.path()), vec!["peer"]);
        let mut s = VecSink(Vec::new());
        c.handle_event(
            Event::Log {
                level: LogLevel::Info,
                message: "peer: delivered a message".into(),
            },
            &mut s,
        );
        c.handle_event(
            Event::Cmd {
                id: 1,
                name: "send_message".into(),
                args: protocol::Args::default(),
            },
            &mut s,
        );
        assert_eq!(
            s.0,
            vec!["$ send_message"],
            "a log line is not a window line"
        );
    }

    /// SPEC's `full` pane title carries what the session runs on, after what the
    /// last request measured.
    ///
    /// # Invariant
    ///
    /// One field per figure, in SPEC's order, each omitted when there is nothing
    /// to say: a session that named neither a directory nor a level shows neither.
    #[test]
    fn a_pane_title_says_what_the_session_runs_on() {
        let session = protocol::SessionInfo {
            name: "main".into(),
            status: SessionStatus::Idle,
            created_at: 0,
            updated_at: 0,
            messages: 0,
            cwd: "C:/work/svc".into(),
            usage: protocol::Usage {
                context_tokens: 2_100,
                cached_tokens: Some(2_000),
                tps: Some(40.0),
                ttft_ms: Some(300),
            },
            provider: Some("deepseek-official".into()),
            thinking_effort: Some("high".into()),
            context_limit: Some(70),
        };
        assert_eq!(
            title_note(&session),
            "ctx 2100 · cached 95% · tps 40.0 · ttft 300ms · cwd C:/work/svc · \
             provider deepseek-official · thinking high · ctx_limit 70%"
        );

        // A session with no tuning of its own still says where it runs.
        let bare = protocol::SessionInfo {
            usage: protocol::Usage::default(),
            cwd: String::new(),
            provider: None,
            thinking_effort: None,
            context_limit: Some(0),
            ..session
        };
        assert_eq!(title_note(&bare), "ctx_limit off");
    }

    /// SPEC's `active_quota` row: the provider, then the figures it reported, in
    /// SPEC's own units.
    #[test]
    fn an_active_quota_row_names_the_provider_and_its_figures() {
        let wallet = protocol::Quota {
            provider: "deepseek-official".into(),
            money: Some(215.52),
            currency: "¥".into(),
            ..protocol::Quota::default()
        };
        assert_eq!(quota_row(&wallet).text, "deepseek-official · money ¥215.52");
        assert_eq!(quota_row(&wallet).kind, LineKind::System);

        // Three windows and a credit pool: one row, SPEC's labels, in its order.
        let plan = protocol::Quota {
            provider: "command-code-goat".into(),
            month_percent: Some(75.4),
            week_percent: Some(40.0),
            five_hour_percent: Some(98.6),
            credit: Some(37.5),
            currency: "$".into(),
            ..protocol::Quota::default()
        };
        assert_eq!(
            quota_row(&plan).text,
            "command-code-goat · month 75% · week 40% · 5h 99% · credit 37.50"
        );

        // A figure the provider did not report is left out, not shown as zero.
        let partial = protocol::Quota {
            provider: "qwen".into(),
            week_percent: Some(12.0),
            ..protocol::Quota::default()
        };
        assert_eq!(quota_row(&partial).text, "qwen · week 12%");
    }

    /// A reading that could not be made is the row's own error, not an empty row.
    #[test]
    fn an_unreadable_quota_row_says_why() {
        let failed = protocol::Quota {
            provider: "minimax-cn".into(),
            note: Some("no credential: set MINIMAX_CN_API_KEY".into()),
            ..protocol::Quota::default()
        };
        let row = quota_row(&failed);
        assert_eq!(
            row.text,
            "minimax-cn · no credential: set MINIMAX_CN_API_KEY"
        );
        assert_eq!(row.kind, LineKind::Error);

        // Figures beside a note stay a fact: the provider flagged one window.
        let flagged = protocol::Quota {
            provider: "command-code-goat".into(),
            week_percent: Some(0.0),
            note: Some("the weekly window is exhausted".into()),
            ..protocol::Quota::default()
        };
        let row = quota_row(&flagged);
        assert_eq!(row.kind, LineKind::System);
        assert!(row.text.ends_with("· the weekly window is exhausted"));
    }

    /// The row reaches the window through the layout, and a repeat of the same
    /// reading is not a second announcement.
    #[test]
    fn a_quota_event_sets_the_layout_row() {
        let d = tempfile::tempdir().unwrap();
        let mut c = client_with(Catalog::new(), Sticky::load(d.path()), vec!["main"]);
        let mut s = StreamSink::default();
        c.refresh(&mut s);
        assert!(s.announced.clone().expect("a layout").quota.is_none());

        let reading = protocol::Quota {
            provider: "deepseek-official".into(),
            money: Some(12.5),
            currency: "¥".into(),
            ..protocol::Quota::default()
        };
        c.handle_event(Event::Quota(reading.clone()), &mut s);
        let announced = s.announced.clone().expect("a layout").quota;
        assert_eq!(
            announced.map(|row| row.text),
            Some("deepseek-official · money ¥12.50".to_string())
        );

        // The same numbers again: nothing to redraw, and the layout is not
        // announced a second time.
        s.announced = None;
        c.handle_event(Event::Quota(reading), &mut s);
        assert!(s.announced.is_none(), "an unchanged reading is not news");
    }

    /// SPEC's three numbers for a `full` pane's title, spelled the way a
    /// terminal reads them.
    ///
    /// # Invariant
    ///
    /// One measurement replaces the whole title: a figure the latest request did
    /// not produce is gone, never left over from the one before it.
    #[test]
    fn a_usage_event_names_the_three_numbers_in_the_title() {
        let d = tempfile::tempdir().unwrap();
        let mut c = client_with(Catalog::new(), Sticky::load(d.path()), vec!["peer"]);
        let mut s = StreamSink::default();
        c.handle_event(
            Event::Usage(protocol::UsageEvent {
                session: "peer".into(),
                usage: protocol::Usage {
                    context_tokens: 12_345,
                    cached_tokens: Some(11_000),
                    tps: Some(41.56),
                    ttft_ms: Some(820),
                },
            }),
            &mut s,
        );
        let announced = s.announced.clone().expect("a layout");
        assert_eq!(
            announced.listed[0].title,
            "ctx 12345 · cached 89% · tps 41.6 · ttft 820ms"
        );

        // The next request reports no count of its own: the rate goes with it.
        c.handle_event(
            Event::Usage(protocol::UsageEvent {
                session: "peer".into(),
                usage: protocol::Usage {
                    context_tokens: 900,
                    cached_tokens: Some(0),
                    tps: None,
                    ttft_ms: Some(1_500),
                },
            }),
            &mut s,
        );
        let announced = s.announced.clone().expect("a layout");
        assert_eq!(announced.listed[0].title, "ctx 900 · cached 0% · ttft 1.5s");

        // A session that has never made a request says nothing rather than zero.
        let quiet = protocol::SessionInfo {
            name: "peer".into(),
            status: SessionStatus::Idle,
            created_at: 0,
            updated_at: 0,
            messages: 0,
            cwd: String::new(),
            usage: protocol::Usage::default(),
            provider: None,
            thinking_effort: None,
            context_limit: None,
        };
        assert_eq!(title_note(&quiet), "");
    }

    /// Captures the status line, which the other sinks throw away.
    #[derive(Debug, Default)]
    struct StatusSink {
        status: String,
    }

    impl Sink for StatusSink {
        fn line(&mut self, _window: &str, _kind: LineKind, _text: &str) {}
        fn stream(
            &mut self,
            _window: &str,
            _k: &str,
            _kind: LineKind,
            _p: &str,
            _text: &str,
            _d: bool,
        ) {
        }
        fn status(&mut self, t: &str) {
            self.status = t.to_string();
        }
    }

    /// One-shot mode renders nothing, so a rejection has to keep its reason.
    #[test]
    fn one_shot_sink_keeps_errors_and_nothing_else() {
        let mut s = NullSink::default();
        s.line("", LineKind::System, "connected to the server");
        s.line(
            "",
            LineKind::Error,
            "send_message is missing required argument(s): session",
        );
        s.stream("", "k", LineKind::Assistant, "", "hello", true);
        s.status("1 session(s)");
        assert_eq!(
            s.take_error().as_deref(),
            Some("send_message is missing required argument(s): session")
        );
        assert_eq!(s.take_error(), None);
    }

    /// The status line states server facts, not a session the UI is "in".
    #[test]
    fn status_line_has_no_current_session() {
        let d = tempfile::tempdir().unwrap();
        let mut sticky = Sticky::load(d.path());
        sticky.set("session", "main").unwrap();
        let c = client_with(Catalog::new(), sticky, vec!["main", "scratch"]);
        let mut s = StatusSink::default();
        c.refresh(&mut s);
        assert!(!s.status.contains("session main"), "status was {s:?}");
        assert!(s.status.contains("2 session(s)"), "status was {s:?}");
    }

    #[test]
    fn sticky_arguments_are_filled_client_side() {
        let d = tempfile::tempdir().unwrap();
        let mut sticky = Sticky::load(d.path());
        sticky.set("session", "scratch").unwrap();
        let cat = Catalog::new();
        cat.set_commands(vec![protocol::CommandSpec {
            target: protocol::CommandTarget::Server,
            name: "send_message".into(),
            description: "s".into(),
            args: vec![
                protocol::ArgSpec {
                    name: "message".into(),
                    description: "s".into(),
                    required: true,
                    value_hint: None,
                    completions: protocol::CompletionSource::None,
                },
                protocol::ArgSpec {
                    name: "session".into(),
                    description: "s".into(),
                    required: true,
                    value_hint: None,
                    completions: protocol::CompletionSource::None,
                },
            ],
        }]);
        let mut c = client_with(cat, sticky, vec!["main", "scratch"]);
        let mut s = VecSink(Vec::new());
        match c.command(argv("send_message --message hi"), &mut s) {
            Submission::Send(ClientMessage::Command { args, .. }) => {
                assert_eq!(args["session"], "scratch");
            }
            _ => panic!("expected a message to send"),
        }
    }

    /// Split puts every part of a session into that session's window.
    #[test]
    fn split_routes_each_part_of_a_session_into_its_window() {
        let d = tempfile::tempdir().unwrap();
        let mut c = client_with(Catalog::new(), Sticky::load(d.path()), vec!["main"]);
        draw_all(&mut c, RenderMode::Full);
        let mut s = StreamSink::default();
        c.handle_response(response("main", ResponsePart::Reasoning, "a"), &mut s);
        c.handle_response(response("main", ResponsePart::ToolCall, "b"), &mut s);
        c.handle_response(response("main", ResponsePart::Output, "c"), &mut s);
        assert_eq!(s.windows, vec!["main", "main", "main"], "{:?}", s.windows);
        assert_eq!(
            s.keys,
            vec!["r:main:1:Reasoning", "r:main:1:ToolCall", "r:main:1:Output"],
            "a part is its own block in the session's window"
        );
        // The pane's header names the speaker; a tool call needs no `tool:`.
        assert_eq!(s.lines, vec!["a", "b", "c"]);
    }

    /// As a line among others, the row's own name says whose it is, so the text
    /// does not repeat it.
    #[test]
    fn a_line_group_row_is_drawn_under_its_own_name() {
        let d = tempfile::tempdir().unwrap();
        let mut c = client_with(Catalog::new(), Sticky::load(d.path()), vec!["main"]);
        c.render.set("main", RenderMode::Line).unwrap();
        let mut s = StreamSink::default();
        c.handle_response(response("main", ResponsePart::Reasoning, "a"), &mut s);
        // The mode is a view, not a rewrite: the window is still the session's.
        assert_eq!(s.windows, vec!["main"]);
        assert_eq!(s.lines, vec!["a"]);
    }

    /// The front-end is told SPEC's three lists: the session list, the panes, the
    /// line groups, and the command line.
    #[test]
    fn the_announcement_is_the_three_lists_of_the_layout() {
        let d = tempfile::tempdir().unwrap();
        let mut c = client_with(Catalog::new(), Sticky::load(d.path()), vec!["main", "peer"]);
        let mut s = StreamSink::default();
        c.refresh(&mut s);
        let first = s.announced.clone().expect("a layout was announced");
        assert_eq!(
            first
                .listed
                .iter()
                .map(|l| l.name.as_str())
                .collect::<Vec<_>>(),
            vec!["main", "peer"],
            "every session is listed whatever its mode: the list is how a mode changes"
        );
        assert_eq!(
            first.full,
            vec!["main", "peer"],
            "a session nobody set is drawn in full"
        );
        assert!(first.line.is_empty(), "nothing is a line group yet");
        assert_eq!(first.command, COMMAND_WINDOW);

        // One session as panes, the other as lines: each list holds its own.
        c.render.set("peer", RenderMode::Line).unwrap();
        c.refresh(&mut s);
        let named = s.announced.clone().expect("a layout was announced");
        assert_eq!(named.full, vec!["main"]);
        assert_eq!(named.line, vec!["peer"]);

        // A session drawn as nothing is in neither pane nor group, and it is what
        // SPEC's session list draws.
        c.render.set("main", RenderMode::None).unwrap();
        c.refresh(&mut s);
        let none = s.announced.clone().expect("a layout was announced");
        assert!(none.full.is_empty());
        assert_eq!(none.line, vec!["peer"]);
        assert_eq!(none.none, vec!["main"], "the list is the mode `none`");
        assert_eq!(none.listed.len(), 2, "and the announcement keeps both");
    }

    /// SPEC gives each render mode one area, so a session is in exactly one of the
    /// three lists however the modes are set.
    ///
    /// # Invariant
    ///
    /// A name drawn in two areas would say the same thing twice, and a name in
    /// none of them would be a session nobody can click.
    #[test]
    fn a_session_is_drawn_in_one_area_only() {
        let d = tempfile::tempdir().unwrap();
        let mut c = client_with(
            Catalog::new(),
            Sticky::load(d.path()),
            vec!["main", "peer", "third"],
        );
        let mut s = StreamSink::default();
        for (peer, third) in [
            (RenderMode::Full, RenderMode::Full),
            (RenderMode::Line, RenderMode::None),
            (RenderMode::None, RenderMode::None),
            (RenderMode::None, RenderMode::Full),
            (RenderMode::Line, RenderMode::Line),
        ] {
            c.render.set("peer", peer).unwrap();
            c.render.set("third", third).unwrap();
            c.refresh(&mut s);
            let layout = s.announced.clone().expect("a layout was announced");
            let mut areas: Vec<&String> = layout
                .none
                .iter()
                .chain(&layout.full)
                .chain(&layout.line)
                .collect();
            // `main` is never set, so it is the default everywhere.
            areas.sort();
            assert_eq!(areas, vec!["main", "peer", "third"], "{layout:?}");
            assert_eq!(layout.listed.len(), 3, "every session stays announced");
        }
    }

    /// A session may be called `cmd`; that is not a reason to lose the command line.
    #[test]
    fn a_session_named_cmd_does_not_take_the_commands_window() {
        let d = tempfile::tempdir().unwrap();
        let mut c = client_with(Catalog::new(), Sticky::load(d.path()), vec!["cmd"]);
        draw_all(&mut c, RenderMode::Full);
        let mut s = StreamSink::default();
        c.handle_event(
            Event::Cmd {
                id: 1,
                name: "get_messages".into(),
                args: protocol::Args::default(),
            },
            &mut s,
        );
        assert_eq!(
            s.windows,
            vec!["cmd "],
            "the line grows a space: {:?}",
            s.windows
        );
        c.refresh(&mut s);
        let named = s.announced.clone().expect("a layout was announced");
        // The session `cmd` is a pane; its name grows a space for the command line.
        assert_eq!(named.full, vec!["cmd"]);
        assert_eq!(
            s.announced.clone().expect("a layout was announced").command,
            "cmd "
        );
    }

    /// Unsplit, a part is a block of its own, so nothing is glued.
    #[test]
    fn unsplit_keeps_one_key_per_part() {
        let d = tempfile::tempdir().unwrap();
        let c = client_with(Catalog::new(), Sticky::load(d.path()), vec!["main"]);
        let mut s = StreamSink::default();
        c.handle_response(response("main", ResponsePart::Reasoning, "a"), &mut s);
        c.handle_response(response("main", ResponsePart::ToolCall, "b"), &mut s);
        assert_eq!(
            s.keys,
            vec!["r:main:1:Reasoning", "r:main:1:ToolCall"],
            "a part names its own block"
        );
    }

    /// Split keeps the command echo and its result on their own lines: they name
    /// no session to join.
    #[test]
    fn split_keeps_the_command_echo_and_its_result_on_their_own_lines() {
        let d = tempfile::tempdir().unwrap();
        let mut c = client_with(Catalog::new(), Sticky::load(d.path()), vec!["main"]);
        draw_all(&mut c, RenderMode::Full);
        let mut s = StreamSink::default();
        c.handle_event(
            Event::Cmd {
                id: 1,
                name: "get_messages".into(),
                args: protocol::Args::default(),
            },
            &mut s,
        );
        c.handle_event(
            Event::CmdResult {
                id: 1,
                ok: true,
                message: "3 rows".into(),
                data: None,
            },
            &mut s,
        );
        assert!(s.keys.is_empty(), "{:?}", s.keys);
        assert_eq!(s.lines, vec!["$ get_messages", "3 rows"]);
        // Both are on the command line, not inside a session's window.
        assert_eq!(s.windows, vec!["cmd", "cmd"]);
    }

    /// An answer names its own subject: the ticker keeps only the last row.
    #[test]
    fn a_window_answer_names_its_own_subject() {
        let d = tempfile::tempdir().unwrap();
        let cat = published_catalog();
        cat.set_sticky_names(&[]);
        let mut c = client_with(cat, Sticky::load(d.path()), vec!["main", "peer"]);
        let mut s = VecSink(Vec::new());

        c.command(argv("set_sticky --name session --value peer"), &mut s);
        assert_eq!(
            s.0.last().map(String::as_str),
            Some("sticky session = peer")
        );
        c.command(argv("del_sticky --name session"), &mut s);
        assert_eq!(
            s.0.last().map(String::as_str),
            Some("sticky session cleared")
        );
        c.command(argv("set_render --block_size 2"), &mut s);
        assert_eq!(
            s.0.last().map(String::as_str),
            Some("render block_size = 2")
        );
        c.command(
            argv("set_render_session --session peer --mode line"),
            &mut s,
        );
        assert_eq!(s.0.last().map(String::as_str), Some("render peer = line"));
    }

    /// `set_render`: one setting, and no argument is a read of it.
    #[test]
    fn set_render_saves_the_block_size_and_reports_it() {
        let d = tempfile::tempdir().unwrap();
        let cat = published_catalog();
        cat.set_sticky_names(&[]);
        let mut c = client_with(cat, Sticky::load(d.path()), vec!["main"]);
        let mut s = VecSink(Vec::new());
        assert!(matches!(
            c.command(argv("set_render --block_size 3"), &mut s),
            Submission::Local
        ));
        assert_eq!(c.block_size, BlockSize(3));
        assert_eq!(BlockSize::load(d.path()), BlockSize(3));
        assert_eq!(
            s.0.last().map(String::as_str),
            Some("render block_size = 3"),
            "the answer names its own subject: the ticker keeps one row"
        );

        // A value nothing answers to is refused, and says what to type.
        s.0.clear();
        c.command(argv("set_render --block_size huge"), &mut s);
        assert_eq!(c.block_size, BlockSize(3));
        let said = s.0.join("\n");
        assert!(said.contains("caps nothing"), "{said}");

        // Without an argument it is a read.
        s.0.clear();
        assert!(matches!(
            c.command(argv("set_render"), &mut s),
            Submission::Local
        ));
        assert_eq!(s.0, vec!["/set_render", "render block_size = 3"]);
    }

    /// `set_render_session`: the mode persists, and the answer is the mode.
    #[test]
    fn set_render_session_saves_one_sessions_mode() {
        let d = tempfile::tempdir().unwrap();
        let cat = published_catalog();
        cat.set_sticky_names(&[]);
        let mut c = client_with(cat, Sticky::load(d.path()), vec!["main", "peer"]);
        let mut s = VecSink(Vec::new());
        assert!(matches!(
            c.command(
                argv("set_render_session --session peer --mode line"),
                &mut s
            ),
            Submission::Local
        ));
        assert_eq!(
            RenderModes::load(d.path()).mode_of("peer"),
            RenderMode::Line
        );
        assert_eq!(s.0.last().map(String::as_str), Some("render peer = line"));

        // A mode nothing answers to is refused, and the list is the answer.
        s.0.clear();
        c.command(
            argv("set_render_session --session peer --mode side"),
            &mut s,
        );
        let said = s.0.join("\n");
        assert!(said.contains("none, line, full"), "{said}");
        assert_eq!(
            RenderModes::load(d.path()).mode_of("peer"),
            RenderMode::Line
        );

        // Without `--mode` it is a read of that session's mode.
        s.0.clear();
        c.command(argv("set_render_session --session peer"), &mut s);
        assert_eq!(s.0.last().map(String::as_str), Some("render peer = line"));
        s.0.clear();
        c.command(argv("set_render_session --session main"), &mut s);
        assert_eq!(s.0.last().map(String::as_str), Some("render main = full"));
    }

    /// Every other result keeps its old look: one line, no invented content.
    #[test]
    fn a_result_without_rows_is_just_its_message() {
        let d = tempfile::tempdir().unwrap();
        let mut c = client_with(Catalog::new(), Sticky::load(d.path()), vec!["main"]);
        let mut s = VecSink(Vec::new());
        for data in [
            None,
            Some(serde_json::json!({"session": "main"})),
            Some(serde_json::json!({"messages": "not rows"})),
        ] {
            c.handle_event(
                Event::CmdResult {
                    id: 1,
                    ok: true,
                    message: "session is now main".into(),
                    data,
                },
                &mut s,
            );
        }
        assert_eq!(s.0, vec!["session is now main"; 3]);
    }

    /// A read's row is the event it was published as, in its session's window.
    #[test]
    fn a_read_back_row_is_a_block_in_its_session_window() {
        let d = tempfile::tempdir().unwrap();
        let mut c = client_with(Catalog::new(), Sticky::load(d.path()), vec!["main"]);

        // A stored tool call reads like the live stream of one: same part, same label.
        let mut s = StreamSink::default();
        let mut call = response("main", ResponsePart::ToolCall, "{\"command\":\"ls\"}");
        call.turn = protocol::REPLAY_TURN;
        call.done = true;
        c.handle_event(Event::Response(call), &mut s);
        assert_eq!(s.windows, vec!["main"]);
        assert_eq!(s.lines, vec!["{\"command\":\"ls\"}"]);

        // Turn 0 keeps two rows of one part in separate blocks, and out of the
        // answer a running turn is still writing.
        let mut s = StreamSink::default();
        let mut first = response("main", ResponsePart::Output, "first row");
        first.turn = protocol::REPLAY_TURN;
        first.done = true;
        let mut second = first.clone();
        second.text = "second row".into();
        c.handle_event(Event::Response(first), &mut s);
        c.handle_event(Event::Response(second), &mut s);
        assert_eq!(
            s.lines,
            vec!["first row", "second row"],
            "one block per row, in the order the store keeps them"
        );

        // A notice is a line the harness wrote, so it travels as a diagnostic.
        let mut s = StreamSink::default();
        c.handle_event(
            Event::Log {
                level: LogLevel::Info,
                message: "failed to run the turn: boom".into(),
            },
            &mut s,
        );
        assert!(
            s.windows.is_empty(),
            "a notice names no window: {:?}",
            s.windows
        );
        assert_eq!(s.notes, vec!["failed to run the turn: boom"]);
    }

    /// A result says how many rows it answered with; the rows are not in it.
    #[test]
    fn a_result_that_answered_with_rows_says_only_how_many() {
        let d = tempfile::tempdir().unwrap();
        let mut c = client_with(Catalog::new(), Sticky::load(d.path()), vec!["main"]);
        draw_all(&mut c, RenderMode::Full);
        let mut s = StreamSink::default();
        c.handle_event(
            Event::CmdResult {
                id: 1,
                ok: true,
                message: "2 row(s) at seq 1..2 in main".into(),
                data: Some(serde_json::json!({"session": "main", "last_seq": 2})),
            },
            &mut s,
        );
        assert_eq!(s.lines, vec!["2 row(s) at seq 1..2 in main"]);
        assert!(
            s.keys.is_empty(),
            "the description is not a row: {:?}",
            s.keys
        );
    }

    /// An unknown flag is refused, and the refusal lists the ones accepted; the
    /// old spelling must not pass silently.
    #[test]
    fn an_unknown_flag_is_refused_and_the_known_ones_are_listed() {
        let d = tempfile::tempdir().unwrap();
        let cat = Catalog::new();
        cat.set_commands(vec![protocol::CommandSpec {
            target: protocol::CommandTarget::Server,
            name: "del_session".into(),
            description: "s".into(),
            args: vec![protocol::ArgSpec {
                name: "session".into(),
                description: "s".into(),
                required: true,
                value_hint: None,
                completions: protocol::CompletionSource::None,
            }],
        }]);
        let mut c = client_with(cat, Sticky::load(d.path()), vec!["main"]);
        let mut s = VecSink(Vec::new());
        // The pre-rename spelling is the case that bit: it used to pass, and a
        // sticky `session` made it look like it worked.
        assert!(matches!(
            c.command(argv("del_session --name main"), &mut s),
            Submission::Rejected
        ));
        let said = s.0.join("\n");
        assert!(said.contains("has no argument --name"), "{said}");
        assert!(said.contains("--session"), "{said}");
    }

    #[test]
    fn client_commands_never_reach_the_wire() {
        let d = tempfile::tempdir().unwrap();
        let cat = published_catalog();
        cat.set_sticky_names(&[]);
        let mut c = client_with(cat, Sticky::load(d.path()), vec!["main"]);
        let mut s = VecSink(Vec::new());
        assert!(matches!(
            c.command(argv("set_sticky --name session --value main"), &mut s),
            Submission::Local
        ));
        assert_eq!(
            c.sticky.pairs(),
            vec![("session".to_string(), "main".to_string())]
        );
        assert!(matches!(
            c.command(argv("del_sticky --name session"), &mut s),
            Submission::Local
        ));
        assert!(c.sticky.pairs().is_empty());
    }

    /// Separate processes should not reuse the same easily guessed id sequence.
    #[test]
    fn two_clients_never_share_a_command_id() {
        let a = Client::seed_command_id();
        let b = Client::seed_command_id();
        assert_ne!(
            a, b,
            "two starts in the same tick still differ by process id"
        );
        assert_ne!(a, 0, "0 is a fine id only if it cannot be a seed");
    }

    /// The whole reason the follow exists: after `set_session` is accepted, a
    /// message needs no `--session` of its own.
    #[test]
    fn a_message_after_set_session_needs_no_session_argument() {
        let d = tempfile::tempdir().unwrap();
        let cat = set_session_catalog();
        let mut with_message = cat.commands();
        with_message.extend(protocol::client_commands());
        with_message.push(protocol::CommandSpec {
            target: protocol::CommandTarget::Server,
            name: "send_message".into(),
            description: "s".into(),
            args: vec![
                protocol::ArgSpec {
                    name: "session".into(),
                    description: "s".into(),
                    required: true,
                    value_hint: None,
                    completions: protocol::CompletionSource::Sessions,
                },
                protocol::ArgSpec {
                    name: "message".into(),
                    description: "s".into(),
                    required: true,
                    value_hint: None,
                    completions: protocol::CompletionSource::None,
                },
            ],
        });
        let cat = Catalog::new();
        cat.set_commands(with_message);
        let mut c = client_with(cat, Sticky::load(d.path()), vec!["main"]);
        let mut s = VecSink(Vec::new());
        c.command(argv("set_session --session main"), &mut s);
        ack(&mut c, true, &mut s);
        match c.message("hello", &mut s) {
            Submission::Send(protocol::ClientMessage::Command { args, .. }) => {
                assert_eq!(args.get("session").map(String::as_str), Some("main"));
            }
            _ => panic!("the message was not sent: {:?}", s.0),
        }
    }

    /// Another client's result settles nothing here, and neither does a result for
    /// a command that named no session.
    #[test]
    fn a_result_that_is_not_this_set_session_writes_nothing() {
        let d = tempfile::tempdir().unwrap();
        let mut c = client_with(
            set_session_catalog(),
            Sticky::load(d.path()),
            vec!["main", "peer"],
        );
        let mut s = VecSink(Vec::new());
        c.command(argv("set_session --session main"), &mut s);
        let ours = c.follows[0].0;
        ack(&mut c, true, &mut s);
        assert_eq!(c.follows.len(), 0);
        // A stranger's id is not a follow any more.
        c.settle_follow(ours + 1, true, &mut s);
        assert_eq!(Sticky::load(d.path()).get("session"), Some("main"));
    }

    /// SPEC: a `set_session` the server accepted becomes the sticky `session`, and
    /// a refused one writes nothing.
    #[test]
    fn an_accepted_set_session_becomes_the_sticky_session() {
        let d = tempfile::tempdir().unwrap();
        let cat = set_session_catalog();
        let mut c = client_with(cat, Sticky::load(d.path()), vec!["main"]);
        let mut s = VecSink(Vec::new());
        assert!(matches!(
            c.command(argv("set_session --session main"), &mut s),
            Submission::Send(_)
        ));
        assert_eq!(
            c.sticky.get("session"),
            None,
            "not before the server says yes"
        );
        ack(&mut c, true, &mut s);
        assert_eq!(c.sticky.get("session"), Some("main"));
        assert_eq!(Sticky::load(d.path()).get("session"), Some("main"));

        // A refusal leaves the client where it was.
        c.command(argv("set_session --session typo"), &mut s);
        ack(&mut c, false, &mut s);
        assert_eq!(c.sticky.get("session"), Some("main"));
    }

    /// A client command is echoed on the command line, where it answers.
    #[test]
    fn a_window_command_is_echoed_where_it_answers() {
        let d = tempfile::tempdir().unwrap();
        let mut c = client_with(
            published_catalog(),
            Sticky::load(d.path()),
            vec!["main", "peer"],
        );
        draw_all(&mut c, RenderMode::Full);
        let mut s = StreamSink::default();
        c.command(argv("hide_session --session peer"), &mut s);
        assert_eq!(
            s.windows,
            vec![COMMAND_WINDOW],
            "a client command is drawn once, where it answers: {:?}",
            s.windows
        );
        // The session's pane is not given a line about the window's own settings.
        assert!(!s.windows.iter().any(|w| w == "peer"), "{:?}", s.windows);
    }

    #[test]
    fn missing_required_arguments_are_reported() {
        let d = tempfile::tempdir().unwrap();
        let cat = Catalog::new();
        cat.set_commands(vec![protocol::CommandSpec {
            target: protocol::CommandTarget::Server,
            name: "del_session".into(),
            description: "s".into(),
            args: vec![protocol::ArgSpec {
                name: "name".into(),
                description: "s".into(),
                required: true,
                value_hint: None,
                completions: protocol::CompletionSource::None,
            }],
        }]);
        let mut c = client_with(cat, Sticky::load(d.path()), vec!["main"]);
        let mut s = VecSink(Vec::new());
        assert!(matches!(
            c.command(argv("del_session"), &mut s),
            Submission::Rejected
        ));
        assert!(s.0.iter().any(|l| l.contains("missing required")));
    }
}
