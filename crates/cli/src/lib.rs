//! `gongga-cli`: the command layer.
//!
//! See `docs/notes/cli-lib.md`.

pub mod catalog;
pub mod commands;
pub mod complete;
pub mod server;

pub use crate::catalog::Catalog;
pub use crate::commands::{BlockSize, Split, Sticky};
pub use crate::complete::{Candidate, Completer, Completion, SharedCompleter, token_bounds};
pub use crate::server::{ensure, starts_here};

use std::collections::HashMap;

use std::net::{SocketAddr, ToSocketAddrs};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, anyhow};
use clap::{CommandFactory, FromArgMatches, Parser};
use protocol::{
    ChannelConfig, ClientInfo, ClientMessage, Event, LogLevel, NO_SESSION, ResponseEvent,
    ResponsePart, ServerMessage, SessionInfo, SessionStatus, ToolAnswerEvent, ToolStream,
    decode_server, encode_client,
};
use tokio::net::UdpSocket;

use crate::commands::{EventFilters, parse_args, render_args};
use protocol::EVENT_FIELDS;

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
#[command(name = "gongga-cli", version)]
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
    pub fn check_flag_order(&self) -> anyhow::Result<()> {
        let Some(Sub::Cmd(command)) = &self.command else {
            return Ok(());
        };
        // Every command flag takes one value, which may itself look like a flag:
        // walk pairs rather than match every token.
        let mut i = 1;
        while i < command.len() {
            let token = &command[i];
            if let Some(flag) = ["--server", "--home", "--key", "--client-only"]
                .iter()
                .find(|flag| token.as_str() == **flag || token.starts_with(&format!("{flag}=")))
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
    let matches = Args::command()
        .name(name)
        .about(about)
        .version(env!("CARGO_PKG_VERSION"))
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
    System,
    User,
    Assistant,
    Reasoning,
    ToolCall,
    ToolOutput,
    ToolError,
    Result,
    Error,
}

/// One events window, as the command layer decided it belongs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Window {
    /// What a line carries to say it belongs here.
    pub name: String,
    /// The single line holding `cmd` and its result: not one of the windows that
    /// share the events area's rows.
    pub command: bool,
}

/// How the events area is divided.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Layout {
    /// One window holding every line in the order it arrived.
    Merged,
    /// One window per name, in this order.
    Split(Vec<Window>),
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
    fn footer(&mut self, text: &str);
    /// How tall one block may get, in wrapped rows; 0 caps nothing. A front-end
    /// that shows every row ignores it.
    fn block_size(&mut self, _lines: usize) {}
    /// How the events area is divided. A front-end that ignores it shows one window.
    fn layout(&mut self, _layout: &Layout) {}
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
    fn footer(&mut self, _text: &str) {}
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
    filters: EventFilters,
    /// How tall one block may get, in wrapped rows.
    block_size: BlockSize,
    /// Whether each session gets its own events window.
    split: Split,
    /// Which window each accepted command's result belongs to, from its echo.
    results: HashMap<u64, String>,
    /// Where `set_session` / `del_session` land, once the server says yes.
    /// See `docs/notes/cli-lib.md`.
    follows: Vec<(u64, String, String)>,
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
    fn show(&self, sink: &mut dyn Sink, echo: &Echo<'_>, tokens: &[String], session: Option<&str>) {
        let window = self.command_line(session);
        match echo {
            Echo::Command => {
                sink.line(&window, LineKind::System, &format!("/{}", tokens.join(" ")))
            }
            Echo::Message(text) => sink.line(&window, LineKind::User, text),
        }
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

    /// What a block opens with: the window says who, the label says which part.
    fn tag(&self, session: &str, label: &str) -> String {
        format!("[{session}] {label}")
    }

    /// How to divide the events area: one window, or one per session.
    ///
    /// # Invariant
    ///
    /// A session filter narrows the panes with the lines, so a session whose events
    /// are not rendered does not keep an empty window on the screen.
    fn layout(&self) -> Layout {
        if !self.split.0 {
            return Layout::Merged;
        }
        let only = self.filters.get("session");
        let mut windows: Vec<Window> = self
            .sessions
            .iter()
            .filter(|s| only.is_none_or(|name| s.name == name))
            .map(|s| Window {
                name: s.name.clone(),
                command: false,
            })
            .collect();
        windows.push(Window {
            name: self.command_window(),
            command: true,
        });
        Layout::Split(windows)
    }
    fn new(catalog: Catalog, sticky: Sticky) -> Self {
        let filters = EventFilters::load(sticky.home());
        let block_size = BlockSize::load(sticky.home());
        let split = Split::load(sticky.home());
        catalog.set_sticky_names(&sticky.names());
        catalog.set_filter_names(&filters.names());
        Self {
            catalog,
            sticky,
            filters,
            block_size,
            split,
            follows: Vec::new(),
            results: HashMap::new(),
            sessions: Vec::new(),
            model: "-".to_string(),
            next_id: Self::seed_command_id(),
            connected: false,
            attempts: 0,
            server_changed: false,
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
                self.sessions = sessions;
                self.catalog.set_commands(commands);
                self.catalog.set_sessions(self.session_names());
                self.sync_sticky();
                // A greeting *is* the recovery: it carries the catalog and
                // sessions, so a redial comes up like a fresh client.
                self.connected = true;
                self.attempts = 0;
                sink.line(
                    &self.command_window(),
                    LineKind::System,
                    &format!("connected to the server (model {})", self.model),
                );
                self.refresh(sink);
            }
            ServerMessage::Denied { reason } => {
                sink.line(
                    &self.command_window(),
                    LineKind::Error,
                    &format!("connection refused: {reason}"),
                );
            }
            ServerMessage::Event(event) => self.handle_event(event, sink),
        }
    }

    fn handle_event(&mut self, event: Event, sink: &mut dyn Sink) {
        // Filters hide lines, not state: a hidden `Status` still updates the list.
        let shown = self.filters.allows(&event);
        match event {
            Event::Cmd { id, name, args } => {
                let window = self.command_line(args.get("session").map(String::as_str));
                // The result carries only the id, so say now where its answer goes.
                self.results.insert(id, window.clone());
                if shown {
                    let line = format!("$ {name}{}", render_args(&args));
                    sink.line(&window, LineKind::System, &line);
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
                self.sessions = sessions;
                self.catalog.set_sessions(self.session_names());
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
            Event::Log { level, message } => {
                if shown {
                    let kind = match level {
                        LogLevel::Debug | LogLevel::Info => LineKind::System,
                        LogLevel::Warn | LogLevel::Error => LineKind::Error,
                    };
                    sink.line(&self.command_window(), kind, &message);
                }
            }
        }
    }

    fn handle_response(&self, e: ResponseEvent, sink: &mut dyn Sink) {
        // One part is one block: the window already keeps the sessions apart.
        let key = format!("r:{}:{}:{:?}", e.session, e.turn, e.part);
        let (kind, label) = match e.part {
            ResponsePart::Reasoning => (LineKind::Reasoning, "thinking: "),
            ResponsePart::ToolCall => (LineKind::ToolCall, "tool: "),
            // Not our tool: the provider ran it, so the label says where it ran.
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

    /// Keeps the completer's `--name` candidates in step with both sets.
    pub fn sync_sticky(&self) {
        self.catalog.set_sticky_names(&self.sticky.names());
        self.catalog.set_filter_names(&self.filters.names());
    }

    /// The status line: server facts only, never a "current session".
    /// See `docs/notes/cli-lib.md`.
    pub fn refresh(&self, sink: &mut dyn Sink) {
        // Told on every refresh, so a front-end attached late still learns it.
        sink.block_size(self.block_size.0);
        sink.layout(&self.layout());
        let facts = if self.connected {
            format!("model {} | {} session(s)", self.model, self.sessions.len())
        } else if self.attempts == 0 {
            "offline — connecting…".to_string()
        } else {
            format!("offline — reconnecting (attempt {})", self.attempts)
        };
        sink.status(&facts);
        let sticky = pairs(&self.sticky.pairs());
        let mut footer = if sticky.is_empty() {
            "sticky: (none)".to_string()
        } else {
            format!("sticky: {sticky}")
        };
        let filters = pairs(&self.filters.pairs());
        if !filters.is_empty() {
            footer.push_str(&format!("  | filters: {filters}"));
        }
        if self.block_size.0 > 0 {
            footer.push_str(&format!("  | block_size: {}", self.block_size.0));
        }
        if self.split.0 {
            footer.push_str(&format!("  | split: {}", self.split.as_str()));
        }
        sink.footer(&footer);
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

        self.show(
            sink,
            &echo,
            &tokens,
            args.get("session").map(String::as_str),
        );
        if spec.target == protocol::CommandTarget::Client {
            self.run_client_command(&head, &args, sink);
            return Submission::Local;
        }

        let id = self.next_id();
        if let Some(session) = args.get("session") {
            if matches!(head.as_str(), "set_session" | "del_session") {
                self.follows.push((id, head.clone(), session.clone()));
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

    /// A refusal has to leave the client where it was, so this runs on the
    /// acknowledgement, not on the submission.
    fn settle_follow(&mut self, id: u64, ok: bool, sink: &mut dyn Sink) {
        let Some(at) = self.follows.iter().position(|(i, ..)| *i == id) else {
            return;
        };
        let (_, name, session) = self.follows.remove(at);
        if ok {
            self.follow_session_command(&name, &session, sink);
        }
    }

    /// `set_session` sets a sticky `session`; `del_session` clears it when it names
    /// that one.
    ///
    /// # Invariant
    ///
    /// The events area is not touched: which session is rendered is a choice the
    /// operator makes with `set_event_ui --session`, not a consequence of which
    /// session a command was sent to.
    fn follow_session_command(&mut self, name: &str, session: &str, sink: &mut dyn Sink) {
        let saved = match name {
            "set_session" => self.sticky.set("session", session),
            "del_session" if self.sticky.get("session") == Some(session) => {
                self.sticky.del("session").map(|_| ())
            }
            _ => return,
        };
        if let Err(e) = saved {
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
                "serve takes over the process; run it in a shell: `gongga serve --port <port>`",
            ),
            "set_event_ui" => self.run_event_ui(args, sink),
            "set_sticky_cmd_argument" => {
                let Some(value) = args.get("value").cloned() else {
                    sink.line(
                        &self.command_window(),
                        LineKind::Error,
                        "set_sticky_cmd_argument requires --value",
                    );
                    return;
                };
                match self.sticky.set(&arg_name, &value) {
                    Ok(()) => {
                        sink.line(
                            &self.command_window(),
                            LineKind::Result,
                            &format!("{arg_name} = {value}"),
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
            "del_sticky_cmd_argument" => match self.sticky.del(&arg_name) {
                Ok(true) => {
                    sink.line(
                        &self.command_window(),
                        LineKind::Result,
                        &format!("cleared {arg_name}"),
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
            "set_event_filter" => {
                let Some(value) = args.get("value").cloned() else {
                    sink.line(
                        &self.command_window(),
                        LineKind::Error,
                        "set_event_filter requires --value",
                    );
                    return;
                };
                if !EVENT_FIELDS.contains(&arg_name.as_str()) {
                    sink.line(
                        &self.command_window(),
                        LineKind::Error,
                        &format!(
                            "no event field named {arg_name:?}; it takes {}",
                            EVENT_FIELDS.join(", ")
                        ),
                    );
                    return;
                }
                match self.filters.set(&arg_name, &value) {
                    Ok(()) => {
                        sink.line(
                            &self.command_window(),
                            LineKind::Result,
                            &format!("{arg_name} = {value}"),
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
            "del_event_filter" => match self.filters.del(&arg_name) {
                Ok(true) => {
                    sink.line(
                        &self.command_window(),
                        LineKind::Result,
                        &format!("cleared filter {arg_name}"),
                    );
                    self.after_sticky_change(sink);
                }
                Ok(false) => sink.line(
                    &self.command_window(),
                    LineKind::Error,
                    &format!("no event filter named {arg_name:?}"),
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

    /// Runs `set_event_ui`: each argument changes one thing about the events area.
    ///
    /// # Invariant
    ///
    /// Every argument parses before any of them saves, so a typo cannot leave the
    /// area configured by half a command, and no argument at all is a read of the
    /// whole setting rather than a reset of it.
    fn run_event_ui(&mut self, args: &protocol::Args, sink: &mut dyn Sink) {
        let mut size = None;
        if let Some(raw) = args.get("block_size") {
            match BlockSize::parse(raw) {
                Ok(parsed) => size = Some(parsed),
                Err(e) => {
                    sink.line(&self.command_window(), LineKind::Error, &e);
                    return;
                }
            }
        }
        let mut split = None;
        if let Some(raw) = args.get("split") {
            match Split::parse(raw) {
                Ok(parsed) => split = Some(parsed),
                Err(e) => {
                    sink.line(&self.command_window(), LineKind::Error, &e);
                    return;
                }
            }
        }
        // A name narrows the lines to that session; `none` renders every one; an
        // empty name is neither.
        let mut session: Option<Option<String>> = None;
        if let Some(raw) = args.get("session") {
            if raw.trim().is_empty() {
                sink.line(
                    &self.command_window(),
                    LineKind::Error,
                    "set_event_ui --session takes a session name, or `none` to render every one",
                );
                return;
            }
            session = Some(if raw.trim() == NO_SESSION {
                None
            } else {
                Some(raw.clone())
            });
        }

        // Nothing named is the question "what is the events area doing?".
        if size.is_none() && split.is_none() && session.is_none() {
            let report = self.event_ui_report();
            sink.line(&self.command_window(), LineKind::Result, &report);
            return;
        }

        let home = self.sticky.home().to_path_buf();
        let mut failed: Vec<String> = Vec::new();
        if let Some(parsed) = size {
            match parsed.save(&home) {
                Ok(()) => self.block_size = parsed,
                Err(e) => failed.push(e.to_string()),
            }
        }
        if let Some(parsed) = split {
            match parsed.save(&home) {
                Ok(()) => self.split = parsed,
                Err(e) => failed.push(e.to_string()),
            }
        }
        if let Some(wanted) = session {
            let saved = match wanted.as_deref() {
                Some(name) => self.filters.set("session", name),
                None => self.filters.del("session").map(|_| ()),
            };
            if let Err(e) = saved {
                failed.push(e.to_string());
            }
        }
        for e in failed {
            sink.line(
                &self.command_window(),
                LineKind::Error,
                &format!("could not save: {e}"),
            );
        }
        let report = self.event_ui_report();
        sink.line(&self.command_window(), LineKind::Result, &report);
        self.after_sticky_change(sink);
    }

    /// The setting as one line, because three answers to one command is an argument.
    fn event_ui_report(&self) -> String {
        format!(
            "block_size = {}  session = {}  split = {}",
            self.block_size.0,
            self.filters.get("session").unwrap_or(NO_SESSION),
            self.split.as_str(),
        )
    }

    fn after_sticky_change(&self, sink: &mut dyn Sink) {
        self.sync_sticky();
        self.refresh(sink);
    }
}

/// Which colour a stored row is drawn in; unknown kinds are context, not errors.
fn pairs(pairs: &[(String, String)]) -> String {
    pairs
        .iter()
        .map(|(k, v)| format!("{k}={v}"))
        .collect::<Vec<_>>()
        .join("  ")
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
                "{}; is a server running? Start one with `gongga serve --port <port>`",
                failure.message()
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
        c.sessions = sessions
            .into_iter()
            .map(|n| SessionInfo {
                name: n.into(),
                status: SessionStatus::Idle,
                created_at: 0,
                updated_at: 0,
                messages: 0,
                cwd: String::new(),
            })
            .collect();
        c
    }

    /// A greeting's catalog: what the server publishes, client commands and all.
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
        fn footer(&mut self, _t: &str) {}
    }

    /// Keeps the window and the prefix, which `VecSink` drops: routing is what the
    /// event tests are about.
    #[derive(Default)]
    struct StreamSink {
        windows: Vec<String>,
        keys: Vec<String>,
        lines: Vec<String>,
        /// The last announcement, which is what a front-end lays out from.
        announced: Option<Layout>,
    }

    impl Sink for StreamSink {
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
        fn footer(&mut self, _t: &str) {}
        fn layout(&mut self, layout: &Layout) {
            self.announced = Some(layout.clone());
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

    /// Every part of a response says who is speaking, before its own label.
    #[test]
    fn a_response_line_names_its_session() {
        let d = tempfile::tempdir().unwrap();
        let c = client_with(Catalog::new(), Sticky::load(d.path()), vec!["peer"]);
        let mut s = StreamSink::default();
        c.handle_response(response("peer", ResponsePart::Reasoning, "hmm"), &mut s);
        c.handle_response(response("peer", ResponsePart::ToolCall, "bash"), &mut s);
        c.handle_response(response("peer", ResponsePart::Output, "hi there"), &mut s);
        assert_eq!(
            s.lines,
            vec![
                "[peer] thinking: hmm",
                "[peer] tool: bash",
                "[peer] hi there"
            ]
        );
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

    /// stdout and stderr both carry the tag, and stay separate streams.
    #[test]
    fn a_tool_answer_line_names_its_session() {
        let d = tempfile::tempdir().unwrap();
        let c = client_with(Catalog::new(), Sticky::load(d.path()), vec!["peer"]);
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
        assert_eq!(s.lines, vec!["[peer] out", "[peer] err"]);
        assert_eq!(s.keys, vec!["t:c1:Stdout", "t:c1:Stderr"]);
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
        assert_eq!(s.0, vec!["peer: delivered a message", "$ send_message"]);
    }

    /// Captures the status line and footer, which the other sinks throw away.
    #[derive(Debug, Default)]
    struct StatusSink {
        status: String,
        footer: String,
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
        fn footer(&mut self, t: &str) {
            self.footer = t.to_string();
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
        s.status("model x | 1 session(s)");
        s.footer("Tab completes");
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
        assert!(s.footer.contains("session=main"), "footer was {s:?}");
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
        c.split = Split(true);
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
        // The label always names the speaker: the merged view has no header.
        assert_eq!(
            s.lines,
            vec!["[main] thinking: a", "[main] tool: b", "[main] c"]
        );
    }

    /// Unsplit is the same traffic with one window and the speaker in the label.
    #[test]
    fn unsplit_puts_everything_in_one_window_and_names_the_speaker() {
        let d = tempfile::tempdir().unwrap();
        let c = client_with(Catalog::new(), Sticky::load(d.path()), vec!["main"]);
        let mut s = StreamSink::default();
        c.handle_response(response("main", ResponsePart::Reasoning, "a"), &mut s);
        // Unsplit is a *view*, not a rewrite of the lines: the window is still named.
        assert_eq!(s.windows, vec!["main"]);
        assert_eq!(s.lines, vec!["[main] thinking: a"]);
    }

    /// The front-end is told what to lay out: one window per session plus the line
    /// the command and its result share.
    #[test]
    fn the_announcement_is_one_window_per_session_plus_the_command_line() {
        let d = tempfile::tempdir().unwrap();
        let mut c = client_with(Catalog::new(), Sticky::load(d.path()), vec!["main", "peer"]);
        let mut s = StreamSink::default();
        c.refresh(&mut s);
        assert_eq!(s.announced, Some(Layout::Merged), "unsplit is one window");

        c.split = Split(true);
        c.refresh(&mut s);
        let named: Vec<(String, bool)> = match s.announced.clone() {
            Some(Layout::Split(windows)) => windows
                .iter()
                .map(|w| (w.name.clone(), w.command))
                .collect(),
            other => panic!("expected a split layout, got {other:?}"),
        };
        assert_eq!(
            named,
            vec![
                ("main".to_string(), false),
                ("peer".to_string(), false),
                ("cmd".to_string(), true)
            ],
            "{named:?}"
        );

        // A session filter narrows the panes with the lines: a window that renders
        // nothing is not a window.
        let mut filters = EventFilters::load(d.path());
        filters.set("session", "main").unwrap();
        c.filters = filters;
        c.refresh(&mut s);
        let names: Vec<String> = match s.announced.clone() {
            Some(Layout::Split(windows)) => windows.iter().map(|w| w.name.clone()).collect(),
            other => panic!("expected a split layout, got {other:?}"),
        };
        assert_eq!(names, vec!["main", "cmd"], "{names:?}");
    }

    /// A session may be called `cmd`; that is not a reason to lose the command line.
    #[test]
    fn a_session_named_cmd_does_not_take_the_commands_window() {
        let d = tempfile::tempdir().unwrap();
        let mut c = client_with(Catalog::new(), Sticky::load(d.path()), vec!["cmd"]);
        c.split = Split(true);
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
        let names: Vec<String> = match s.announced.clone() {
            Some(Layout::Split(windows)) => windows.iter().map(|w| w.name.clone()).collect(),
            other => panic!("expected a split layout, got {other:?}"),
        };
        assert_eq!(names, vec!["cmd", "cmd "]);
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
        c.split = Split(true);
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

    /// One command's three arguments: each persists, and the answer is the setting.
    #[test]
    fn set_event_ui_saves_and_tells_the_front_end() {
        let d = tempfile::tempdir().unwrap();
        let cat = published_catalog();
        cat.set_sticky_names(&[]);
        cat.set_filter_names(&[]);
        let mut c = client_with(cat, Sticky::load(d.path()), vec!["main"]);
        let mut s = VecSink(Vec::new());
        assert!(matches!(
            c.command(argv("set_event_ui --block_size 3"), &mut s),
            Submission::Local
        ));
        assert_eq!(c.block_size, BlockSize(3));
        assert_eq!(BlockSize::load(d.path()), BlockSize(3));

        // Split and the session filter are its other two arguments, and one line
        // answers all three.
        assert!(matches!(
            c.command(argv("set_event_ui --split true --session main"), &mut s),
            Submission::Local
        ));
        assert_eq!(c.split, Split(true));
        assert_eq!(Split::load(d.path()), Split(true));
        assert_eq!(c.filters.get("session"), Some("main"));
        assert_eq!(
            s.0.last().map(String::as_str),
            Some("block_size = 3  session = main  split = true"),
            "{:?}",
            s.0
        );

        // A value nothing answers to is refused, and the answer says what to type.
        s.0.clear();
        c.command(argv("set_event_ui --block_size huge"), &mut s);
        let said = s.0.join("\n");
        assert!(said.contains("caps nothing"), "{said}");

        // One bad argument vetoes the good one beside it: half an applied command
        // is an area nobody chose.
        s.0.clear();
        c.command(argv("set_event_ui --block_size 9 --split sideways"), &mut s);
        assert_eq!(c.block_size, BlockSize(3), "the bad split vetoed the size");
        assert_eq!(c.split, Split(true));
        let said = s.0.join("\n");
        assert!(said.contains("true, false"), "{said}");
        assert!(
            !said.contains("block_size ="),
            "a refused setting is not reported: {said}"
        );

        // Without an argument it is a read: the setting is reported, not reset.
        s.0.clear();
        assert!(matches!(
            c.command(argv("set_event_ui"), &mut s),
            Submission::Local
        ));
        assert_eq!(c.block_size, BlockSize(3));
        assert_eq!(
            s.0,
            vec![
                "/set_event_ui",
                "block_size = 3  session = main  split = true"
            ]
        );

        // `none` is the word that stops filtering, not a session called none.
        assert!(matches!(
            c.command(argv("set_event_ui --session none"), &mut s),
            Submission::Local
        ));
        assert_eq!(c.filters.get("session"), None);
        assert_eq!(
            s.0.last().map(String::as_str),
            Some("block_size = 3  session = none  split = true")
        );
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
        assert_eq!(s.lines, vec!["[main] tool: {\"command\":\"ls\"}"]);

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
            vec!["[main] first row", "[main] second row"],
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
        assert_eq!(s.windows, vec!["cmd"]);
        assert_eq!(s.lines, vec!["failed to run the turn: boom"]);
    }

    /// A result says how many rows it answered with; the rows are not in it.
    #[test]
    fn a_result_that_answered_with_rows_says_only_how_many() {
        let d = tempfile::tempdir().unwrap();
        let mut c = client_with(Catalog::new(), Sticky::load(d.path()), vec!["main"]);
        c.split = Split(true);
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

    #[test]
    fn client_commands_never_reach_the_wire() {
        let d = tempfile::tempdir().unwrap();
        let cat = published_catalog();
        cat.set_sticky_names(&[]);
        let mut c = client_with(cat, Sticky::load(d.path()), vec!["main"]);
        let mut s = VecSink(Vec::new());
        assert!(matches!(
            c.command(
                argv("set_sticky_cmd_argument --name session --value main"),
                &mut s
            ),
            Submission::Local
        ));
        assert_eq!(
            c.sticky.pairs(),
            vec![("session".to_string(), "main".to_string())]
        );
        assert!(matches!(
            c.command(argv("del_sticky_cmd_argument --name session"), &mut s),
            Submission::Local
        ));
        assert!(c.sticky.pairs().is_empty());
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

    /// A session filter silences the other sessions' streams and nothing else.
    #[test]
    fn an_event_filter_hides_the_streams_it_does_not_name() {
        let d = tempfile::tempdir().unwrap();
        let mut filters = EventFilters::load(d.path());
        filters.set("session", "main").unwrap();
        let mut c = Client::new(Catalog::new(), Sticky::load(d.path()));
        c.filters = filters;
        c.connected = true;
        let mut s = StreamSink::default();
        for session in ["main", "peer"] {
            let text = if session == "main" { "mine" } else { "theirs" };
            c.handle_event(
                Event::Response(response(session, ResponsePart::Output, text)),
                &mut s,
            );
        }
        assert_eq!(s.lines, vec!["[main] mine"]);
    }

    /// A filter is written with a wire field name, so this goes through bytes.
    #[test]
    fn a_filter_matches_an_event_that_arrived_on_the_wire() {
        let d = tempfile::tempdir().unwrap();
        let mut filters = EventFilters::load(d.path());
        filters.set("session", "main").unwrap();
        let mut c = client_with(Catalog::new(), Sticky::load(d.path()), vec!["main"]);
        c.filters = filters;
        let mut s = StreamSink::default();

        for session in ["main", "peer"] {
            let msg = ServerMessage::Event(Event::Response(response(
                session,
                ResponsePart::Output,
                session,
            )));
            c.handle_bytes(&encode_server(&msg).unwrap(), &mut s)
                .unwrap();
        }
        assert_eq!(s.lines, vec!["[main] main"]);
    }

    /// Filters are about what is shown: the session list still learns the
    /// status of a session whose lines are hidden.
    #[test]
    fn a_filtered_event_still_updates_the_client_state() {
        let d = tempfile::tempdir().unwrap();
        let mut filters = EventFilters::load(d.path());
        filters.set("session", "main").unwrap();
        let mut c = client_with(Catalog::new(), Sticky::load(d.path()), vec!["peer"]);
        c.filters = filters;
        let mut s = VecSink(Vec::new());
        c.handle_event(
            Event::Status {
                session: "peer".into(),
                status: SessionStatus::Running,
                detail: None,
            },
            &mut s,
        );
        assert!(s.0.is_empty(), "the line was hidden: {:?}", s.0);
        assert_eq!(c.sessions[0].status, SessionStatus::Running);
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

    /// A result addressed to another client settles nothing here.
    #[test]
    fn a_result_for_another_client_is_not_ours() {
        let d = tempfile::tempdir().unwrap();
        let mut c = client_with(Catalog::new(), Sticky::load(d.path()), vec!["main"]);
        let id = c.next_id();
        // The pending follow belongs to *our* id; a stranger's must not clear it.
        c.follows
            .push((id, "set_session".to_string(), "main".to_string()));
        let mut s = VecSink(Vec::new());
        c.settle_follow(id + 1, true, &mut s);
        assert_eq!(c.follows.len(), 1, "a foreign id settled our follow");
        c.settle_follow(id, true, &mut s);
        assert!(c.follows.is_empty());
    }

    /// The acknowledgement of the command just sent, so `settle_follow` sees it.
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

    /// Naming a session is what later commands act on -- not what this client renders.
    #[test]
    fn set_session_is_a_target_not_a_filter() {
        let d = tempfile::tempdir().unwrap();
        let cat = Catalog::new();
        cat.set_commands(vec![protocol::CommandSpec {
            target: protocol::CommandTarget::Server,
            name: "set_session".into(),
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
        assert!(matches!(
            c.command(argv("set_session --session main"), &mut s),
            Submission::Send(_)
        ));
        ack(&mut c, true, &mut s);
        assert_eq!(c.sticky.get("session"), Some("main"));
        assert_eq!(
            c.filters.get("session"),
            None,
            "which session to render is `set_event_ui --session`'s to say"
        );
    }

    /// A refused `set_session` must not leave the client watching a session that
    /// was never created.
    #[test]
    fn a_refused_set_session_leaves_the_client_where_it_was() {
        let d = tempfile::tempdir().unwrap();
        let cat = Catalog::new();
        cat.set_commands(vec![protocol::CommandSpec {
            target: protocol::CommandTarget::Server,
            name: "set_session".into(),
            description: "s".into(),
            args: vec![protocol::ArgSpec {
                name: "session".into(),
                description: "s".into(),
                required: true,
                value_hint: None,
                completions: protocol::CompletionSource::None,
            }],
        }]);
        let mut sticky = Sticky::load(d.path());
        sticky.set("session", "main").unwrap();
        let mut c = client_with(cat, sticky, vec!["main"]);
        let mut s = VecSink(Vec::new());
        c.command(argv("set_session --session typo"), &mut s);
        ack(&mut c, false, &mut s);
        assert_eq!(c.sticky.get("session"), Some("main"));
        assert_eq!(c.filters.get("session"), None);
    }

    /// Deleting another session must not retarget the client.
    #[test]
    fn del_session_only_clears_the_session_it_names() {
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
        let mut sticky = Sticky::load(d.path());
        sticky.set("session", "main").unwrap();
        let mut filters = EventFilters::load(d.path());
        filters.set("session", "main").unwrap();
        let mut c = client_with(cat, sticky, vec!["main", "scratch"]);
        c.filters = filters;

        let mut s = VecSink(Vec::new());
        c.command(argv("del_session --session scratch"), &mut s);
        ack(&mut c, true, &mut s);
        assert_eq!(c.sticky.get("session"), Some("main"));
        assert_eq!(c.filters.get("session"), Some("main"));

        c.command(argv("del_session --session main"), &mut s);
        ack(&mut c, true, &mut s);
        assert_eq!(c.sticky.get("session"), None);
        // An operator set that filter, not this command: it stays until
        // `set_event_ui --session none` says otherwise.
        assert_eq!(c.filters.get("session"), Some("main"));
    }

    /// A field no event carries would filter nothing; say so instead of
    /// storing a filter that quietly does nothing.
    #[test]
    fn set_event_filter_refuses_a_field_no_event_has() {
        let d = tempfile::tempdir().unwrap();
        let mut c = client_with(published_catalog(), Sticky::load(d.path()), vec!["main"]);
        let mut s = VecSink(Vec::new());
        assert!(matches!(
            c.command(argv("set_event_filter --name nope --value x"), &mut s),
            Submission::Local
        ));
        assert!(c.filters.pairs().is_empty());
        let said = s.0.join("\n");
        assert!(said.contains("no event field named \"nope\""), "{said}");
        assert!(said.contains("session"), "{said}");
    }

    /// A filter that hides lines has to be visible somewhere, or the log reads
    /// as broken.
    #[test]
    fn the_footer_shows_the_filters() {
        let d = tempfile::tempdir().unwrap();
        let mut c = client_with(Catalog::new(), Sticky::load(d.path()), vec!["main"]);
        let mut s = StatusSink::default();
        c.refresh(&mut s);
        assert!(!s.footer.contains("filters"), "footer was {s:?}");

        let mut filters = EventFilters::load(d.path());
        filters.set("session", "main").unwrap();
        c.filters = filters;
        c.refresh(&mut s);
        assert!(s.footer.contains("filters: session=main"), "{s:?}");
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
