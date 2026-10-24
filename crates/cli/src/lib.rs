//! `gongga-cli`: the command layer.
//!
//! See `docs/notes/cli-lib.md`.

pub mod catalog;
pub mod commands;
pub mod complete;

pub use crate::catalog::Catalog;
pub use crate::commands::{Detail, Sticky};
pub use crate::complete::{Candidate, Completer, Completion, SharedCompleter, token_bounds};

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, anyhow};
use clap::Parser;
use protocol::{
    ChannelConfig, ClientInfo, ClientMessage, Event, LogLevel, ResponseEvent, ResponsePart,
    ServerMessage, SessionStatus, SessionSummary, ToolAnswerEvent, ToolStream, decode_server,
    encode_client,
};
use tokio::net::UdpSocket;

use crate::commands::{EVENT_FIELDS, EventFilters, parse_args, render_args};

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
#[command(name = "gongga", about = "Gongga harness terminal client", version)]
pub struct Args {
    #[command(flatten)]
    pub conn: Connection,
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
        if let Some(flag) = command.iter().find(|t| {
            ["--server", "--home", "--key"]
                .iter()
                .any(|f| *t == f || t.starts_with(&format!("{f}=")))
        }) {
            anyhow::bail!(
                "{flag} has to come before the command: `gongga {flag} <value> {} ...`",
                command[0]
            );
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

/// Parses the command line, the one way both binaries do it.
pub fn parse_argv() -> Args {
    Args::parse()
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

/// Everything the client can ask a front-end to show; neither implementation
/// knows what produced the text.
pub trait Sink {
    fn line(&mut self, kind: LineKind, text: &str);
    fn stream(&mut self, key: &str, kind: LineKind, prefix: &str, text: &str, done: bool);
    fn status(&mut self, text: &str);
    fn footer(&mut self, text: &str);
    /// How much of each event to show. A front-end that does not fold ignores it.
    fn detail(&mut self, _mode: Detail) {}
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
    fn line(&mut self, kind: LineKind, text: &str) {
        if matches!(kind, LineKind::Error) {
            self.errors.push(text.to_string());
        }
    }
    fn stream(&mut self, _key: &str, _kind: LineKind, _prefix: &str, _text: &str, _done: bool) {}
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
    detail: Detail,
    /// Where `set_session` / `del_session` land, once the server says yes.
    /// See `docs/notes/cli-lib.md`.
    follows: Vec<(u64, String, String)>,
    sessions: Vec<SessionSummary>,
    model: String,
    next_id: u64,
    /// Whether a link is open right now; while false, all server knowledge is
    /// stale.
    connected: bool,
    /// Failed dials since the link went down, so silence is legible.
    attempts: u32,
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

impl Echo<'_> {
    fn show(&self, sink: &mut dyn Sink, tokens: &[String]) {
        match self {
            Echo::Command => sink.line(LineKind::System, &format!("/{}", tokens.join(" "))),
            Echo::Message(text) => sink.line(LineKind::User, text),
        }
    }
}

impl Client {
    fn new(catalog: Catalog, sticky: Sticky) -> Self {
        let filters = EventFilters::load(sticky.home());
        let detail = Detail::load(sticky.home());
        catalog.set_sticky_names(&sticky.names());
        catalog.set_filter_names(&filters.names());
        Self {
            catalog,
            sticky,
            filters,
            detail,
            follows: Vec::new(),
            sessions: Vec::new(),
            model: "-".to_string(),
            next_id: 1,
            connected: false,
            attempts: 0,
        }
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
                self.catalog.set_server_commands(commands);
                self.catalog.set_sessions(self.session_names());
                self.sync_sticky();
                // A greeting *is* the recovery: it carries the catalog and
                // sessions, so a redial comes up like a fresh client.
                self.connected = true;
                self.attempts = 0;
                sink.line(
                    LineKind::System,
                    &format!("connected to gongga-server (model {})", self.model),
                );
                self.refresh(sink);
            }
            ServerMessage::Denied { reason } => {
                sink.line(LineKind::Error, &format!("connection refused: {reason}"));
            }
            ServerMessage::Event(event) => self.handle_event(event, sink),
        }
    }

    fn handle_event(&mut self, event: Event, sink: &mut dyn Sink) {
        // Filters hide lines, not state: a hidden `Status` still updates the list.
        let shown = self.filters.allows(&event);
        match event {
            Event::Cmd { name, args, .. } => {
                if shown {
                    let line = format!("$ {name}{}", render_args(&args));
                    self.cmd_line(LineKind::System, &line, sink);
                }
            }
            Event::CmdResult {
                id, message, ok, ..
            } => {
                self.settle_follow(id, ok, sink);
                if shown && !message.is_empty() {
                    let kind = if ok {
                        LineKind::Result
                    } else {
                        LineKind::Error
                    };
                    self.cmd_line(kind, &message, sink);
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
                    sink.line(LineKind::System, &format!("[{session}] {label}{suffix}"));
                }
                self.refresh(sink);
            }
            Event::Log { level, message } => {
                if shown {
                    let kind = match level {
                        LogLevel::Debug | LogLevel::Info => LineKind::System,
                        LogLevel::Warn | LogLevel::Error => LineKind::Error,
                    };
                    sink.line(kind, &message);
                }
            }
        }
    }

    fn handle_response(&self, e: ResponseEvent, sink: &mut dyn Sink) {
        // Which rows a stream joins is meaning, so it is decided here; how a
        // block is drawn is the front-end's.
        let key = match (self.detail, e.part) {
            // `one` folds a session's messages together; the answer keeps its text.
            (Detail::One, ResponsePart::Output) => format!("o:{}:{}", e.session, e.turn),
            (Detail::One, _) => format!("s:{}", e.session),
            _ => format!("r:{}:{}:{:?}", e.session, e.turn, e.part),
        };
        let (kind, label) = match e.part {
            ResponsePart::Reasoning => (LineKind::Reasoning, "thinking: "),
            ResponsePart::ToolCall => (LineKind::ToolCall, "tool: "),
            // Not our tool: the provider ran it, so the label says where it ran.
            ResponsePart::Activity => (LineKind::ToolCall, "provider: "),
            ResponsePart::Output => (LineKind::Assistant, ""),
        };
        // The tag rides on the prefix: a prefix is printed once per key.
        let prefix = format!("[{}] {label}", e.session);
        sink.stream(&key, kind, &prefix, &e.text, e.done);
    }

    /// The command echo and its result: one row each, or one shared row in `one`.
    fn cmd_line(&self, kind: LineKind, text: &str, sink: &mut dyn Sink) {
        if self.detail == Detail::One {
            sink.stream("cmd", kind, "", text, false);
        } else {
            sink.line(kind, text);
        }
    }

    fn handle_tool_answer(&self, e: ToolAnswerEvent, sink: &mut dyn Sink) {
        let key = format!("t:{}:{:?}", e.call_id, e.stream);
        let kind = match e.stream {
            ToolStream::Stdout => LineKind::ToolOutput,
            ToolStream::Stderr => LineKind::ToolError,
        };
        // Same shape as a `Status` line: who is speaking, then the output.
        let prefix = format!("[{}] ", e.session);
        sink.stream(&key, kind, &prefix, &e.text, e.done);
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
        sink.detail(self.detail);
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
        if self.detail != Detail::All {
            footer.push_str(&format!("  | detail: {}", self.detail.as_str()));
        }
        sink.footer(&footer);
    }

    /// Records that there is no link, and how many dials have failed since.
    pub fn set_offline(&mut self, attempts: u32, sink: &mut dyn Sink) {
        self.connected = false;
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
            sink.line(
                LineKind::Error,
                &format!(
                    "unknown command {head:?}; this client knows: {}",
                    known.join(", ")
                ),
            );
            return Submission::Rejected;
        };

        if self.catalog.is_client_command(&head) {
            echo.show(sink, &tokens);
            self.run_client_command(&head, &tokens[1..], sink);
            return Submission::Local;
        }

        echo.show(sink, &tokens);
        let mut args = match parse_args(&tokens[1..]) {
            Ok(a) => a,
            Err(e) => {
                sink.line(LineKind::Error, &e.to_string());
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
            sink.line(
                LineKind::Error,
                &format!(
                    "{} is missing required argument(s): {}",
                    head,
                    missing.join(", ")
                ),
            );
            return Submission::Rejected;
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

    /// `set_session` sets a sticky `session` and its filter; `del_session` clears
    /// them only when it names that session.
    fn follow_session_command(&mut self, name: &str, session: &str, sink: &mut dyn Sink) {
        let saved = match name {
            "set_session" => self
                .sticky
                .set("session", session)
                .and_then(|()| self.filters.set("session", session)),
            "del_session" => {
                let sticky = match self.sticky.get("session") == Some(session) {
                    true => self.sticky.del("session"),
                    false => Ok(false),
                };
                let filters = match self.filters.get("session") == Some(session) {
                    true => self.filters.del("session"),
                    false => Ok(false),
                };
                sticky.and(filters).map(|_| ())
            }
            _ => return,
        };
        if let Err(e) = saved {
            sink.line(LineKind::Error, &format!("could not save: {e}"));
        }
        self.after_sticky_change(sink);
    }

    fn run_client_command(&mut self, name: &str, tokens: &[String], sink: &mut dyn Sink) {
        let args = match parse_args(tokens) {
            Ok(a) => a,
            Err(e) => {
                sink.line(LineKind::Error, &e.to_string());
                return;
            }
        };
        // `--name` belongs to the commands that name something; the detail mode
        // names a mode instead.
        if !matches!(name, "set_event_detail") && !args.contains_key("name") {
            sink.line(LineKind::Error, &format!("{name} requires --name"));
            return;
        }
        let arg_name = args.get("name").cloned().unwrap_or_default();
        match name {
            "set_event_detail" => {
                let Some(raw) = args.get("mode").cloned() else {
                    sink.line(LineKind::Error, "set_event_detail requires --mode");
                    return;
                };
                let mode = match Detail::parse(&raw) {
                    Ok(m) => m,
                    Err(e) => {
                        sink.line(LineKind::Error, &e);
                        return;
                    }
                };
                match mode.save(self.sticky.home()) {
                    Ok(()) => {
                        self.detail = mode;
                        sink.line(LineKind::Result, &format!("detail = {}", mode.as_str()));
                        self.after_sticky_change(sink);
                    }
                    Err(e) => sink.line(LineKind::Error, &format!("could not save: {e}")),
                }
            }
            "set_persistant_cmd_argument" => {
                let Some(value) = args.get("value").cloned() else {
                    sink.line(
                        LineKind::Error,
                        "set_persistant_cmd_argument requires --value",
                    );
                    return;
                };
                match self.sticky.set(&arg_name, &value) {
                    Ok(()) => {
                        sink.line(LineKind::Result, &format!("{arg_name} = {value}"));
                        self.after_sticky_change(sink);
                    }
                    Err(e) => sink.line(LineKind::Error, &format!("could not save: {e}")),
                }
            }
            "del_persistant_cmd_argument" => match self.sticky.del(&arg_name) {
                Ok(true) => {
                    sink.line(LineKind::Result, &format!("cleared {arg_name}"));
                    self.after_sticky_change(sink);
                }
                Ok(false) => sink.line(
                    LineKind::Error,
                    &format!("no sticky argument named {arg_name:?}"),
                ),
                Err(e) => sink.line(LineKind::Error, &format!("could not save: {e}")),
            },
            "set_event_filter" => {
                let Some(value) = args.get("value").cloned() else {
                    sink.line(LineKind::Error, "set_event_filter requires --value");
                    return;
                };
                if !EVENT_FIELDS.contains(&arg_name.as_str()) {
                    sink.line(
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
                        sink.line(LineKind::Result, &format!("{arg_name} = {value}"));
                        self.after_sticky_change(sink);
                    }
                    Err(e) => sink.line(LineKind::Error, &format!("could not save: {e}")),
                }
            }
            "del_event_filter" => match self.filters.del(&arg_name) {
                Ok(true) => {
                    sink.line(LineKind::Result, &format!("cleared filter {arg_name}"));
                    self.after_sticky_change(sink);
                }
                Ok(false) => sink.line(
                    LineKind::Error,
                    &format!("no event filter named {arg_name:?}"),
                ),
                Err(e) => sink.line(LineKind::Error, &format!("could not save: {e}")),
            },
            other => sink.line(
                LineKind::Error,
                &format!("unknown client command {other:?}"),
            ),
        }
    }

    fn after_sticky_change(&self, sink: &mut dyn Sink) {
        self.sync_sticky();
        self.refresh(sink);
    }
}

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

/// Headless: one command in, its JSON events out, then exit.
///
/// See `docs/notes/cli-lib.md`.
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

// Connection

/// A resolved address and key, fixed for the life of the process.
#[derive(Clone)]
pub struct Endpoint {
    addr: SocketAddr,
    key: String,
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
    /// Worth retrying: the server is not listening right now.
    Temporary(anyhow::Error),
}

impl DialFailure {
    pub fn message(&self) -> String {
        match self {
            DialFailure::Fatal(e) | DialFailure::Temporary(e) => e.to_string(),
        }
    }

    pub fn is_fatal(&self) -> bool {
        matches!(self, DialFailure::Fatal(_))
    }

    pub fn into_error(self) -> anyhow::Error {
        match self {
            DialFailure::Fatal(e) | DialFailure::Temporary(e) => e,
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
        })?
        .ok_or_else(|| {
            DialFailure::Temporary(anyhow!(
                "{} closed the connection during the handshake",
                endpoint.addr
            ))
        })?;

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
                "{}; is `gongga-server` running?",
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
    if let Some(bind) = config_value(args, |c| c.server.as_ref().and_then(|s| s.bind.clone())) {
        return bind
            .parse()
            .with_context(|| format!("parsing bind {bind:?}"));
    }
    Ok(DEFAULT_SERVER.parse().unwrap())
}

/// `--key`, then `GONGGA_KEY`, then `[server] key` in config.toml.
fn resolve_key(args: &Connection) -> Option<String> {
    resolve_key_with(args, &process_env)
}

fn resolve_key_with(args: &Connection, env: &Env) -> Option<String> {
    if let Some(k) = &args.key {
        return Some(k.clone());
    }
    if let Some(k) = env("GONGGA_KEY") {
        return Some(k);
    }
    config_value(args, |c| c.server.as_ref().and_then(|s| s.key.clone()))
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
    let path = home_dir(args.home.as_deref()).join("config.toml");
    let text = std::fs::read_to_string(path).ok()?;
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
        // And no `[[bin]]` either: the binary is the composition root, compared
        // as a whole line so a comment cannot match.
        assert!(
            !manifest.lines().any(|l| l.trim() == "[[bin]]"),
            "{manifest}"
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
            .map(|n| SessionSummary {
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

    /// The smallest catalog that can take a message: one command, one required
    /// argument, one sticky one.
    fn send_message_catalog() -> Catalog {
        let cat = Catalog::new();
        cat.set_server_commands(vec![protocol::CommandSpec {
            name: "send_message".into(),
            summary: "s".into(),
            args: vec![
                protocol::ArgSpec {
                    name: "message".into(),
                    summary: "s".into(),
                    required: true,
                    value_hint: None,
                    completions: protocol::CompletionSource::None,
                },
                protocol::ArgSpec {
                    name: "session".into(),
                    summary: "s".into(),
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
        fn line(&mut self, _kind: LineKind, text: &str) {
            self.0.push(text.to_string());
        }
        fn stream(&mut self, _k: &str, _kind: LineKind, _p: &str, text: &str, _d: bool) {
            self.0.push(text.to_string());
        }
        fn status(&mut self, _t: &str) {}
        fn footer(&mut self, _t: &str) {}
    }

    /// Keeps the prefix, which `VecSink` drops: it reads the text alone.
    #[derive(Default)]
    struct StreamSink {
        keys: Vec<String>,
        lines: Vec<String>,
    }

    impl Sink for StreamSink {
        fn line(&mut self, _kind: LineKind, _text: &str) {}
        fn stream(&mut self, key: &str, _kind: LineKind, prefix: &str, text: &str, _done: bool) {
            self.keys.push(key.to_string());
            self.lines.push(format!("{prefix}{text}"));
        }
        fn status(&mut self, _t: &str) {}
        fn footer(&mut self, _t: &str) {}
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
        fn line(&mut self, _kind: LineKind, _text: &str) {}
        fn stream(&mut self, _k: &str, _kind: LineKind, _p: &str, _text: &str, _d: bool) {}
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
        s.line(LineKind::System, "connected to gongga-server");
        s.line(
            LineKind::Error,
            "send_message is missing required argument(s): session",
        );
        s.stream("k", LineKind::Assistant, "", "hello", true);
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
        cat.set_server_commands(vec![protocol::CommandSpec {
            name: "send_message".into(),
            summary: "s".into(),
            args: vec![
                protocol::ArgSpec {
                    name: "message".into(),
                    summary: "s".into(),
                    required: true,
                    value_hint: None,
                    completions: protocol::CompletionSource::None,
                },
                protocol::ArgSpec {
                    name: "session".into(),
                    summary: "s".into(),
                    required: false,
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

    /// `one` groups a session's messages under one key so the front-end can fold
    /// them; the other modes keep one key per part.
    #[test]
    fn one_mode_groups_a_sessions_messages_under_one_key() {
        let d = tempfile::tempdir().unwrap();
        let mut c = client_with(Catalog::new(), Sticky::load(d.path()), vec!["main"]);
        c.detail = Detail::One;
        let mut s = StreamSink::default();
        c.handle_response(response("main", ResponsePart::Reasoning, "a"), &mut s);
        c.handle_response(response("main", ResponsePart::ToolCall, "b"), &mut s);
        c.handle_response(response("main", ResponsePart::Output, "c"), &mut s);
        // Reasoning and tool calls share the session's row; the answer has its own.
        assert_eq!(s.keys, vec!["s:main", "s:main", "o:main:1"]);
    }

    /// In the other modes a part is a block of its own, so nothing is glued.
    #[test]
    fn all_and_line_keep_one_key_per_part() {
        let d = tempfile::tempdir().unwrap();
        let mut c = client_with(Catalog::new(), Sticky::load(d.path()), vec!["main"]);
        for mode in [Detail::All, Detail::Line] {
            c.detail = mode;
            let mut s = StreamSink::default();
            c.handle_response(response("main", ResponsePart::Reasoning, "a"), &mut s);
            c.handle_response(response("main", ResponsePart::ToolCall, "b"), &mut s);
            assert_eq!(
                s.keys,
                vec!["r:main:1:Reasoning", "r:main:1:ToolCall"],
                "{mode:?}"
            );
        }
    }

    /// `one` puts the command echo and its result on one shared row.
    #[test]
    fn one_mode_folds_the_command_echo_and_its_result() {
        let d = tempfile::tempdir().unwrap();
        let mut c = client_with(Catalog::new(), Sticky::load(d.path()), vec!["main"]);
        c.detail = Detail::One;
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
        assert_eq!(s.keys, vec!["cmd", "cmd"]);
        assert_eq!(s.lines, vec!["$ get_messages", "3 rows"]);

        // The other modes keep them as lines of their own.
        c.detail = Detail::All;
        let mut s = StreamSink::default();
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
    }

    /// The mode is a client setting: it persists and the front-end is told.
    #[test]
    fn set_event_detail_saves_and_tells_the_front_end() {
        let d = tempfile::tempdir().unwrap();
        let cat = Catalog::new();
        cat.set_sticky_names(&[]);
        cat.set_filter_names(&[]);
        let mut c = client_with(cat, Sticky::load(d.path()), vec!["main"]);
        let mut s = VecSink(Vec::new());
        assert!(matches!(
            c.command(argv("set_event_detail --mode line"), &mut s),
            Submission::Local
        ));
        assert_eq!(c.detail, Detail::Line);
        assert_eq!(Detail::load(d.path()), Detail::Line);

        // A mode nothing answers to is refused, and the list says what does.
        assert!(matches!(
            c.command(argv("set_event_detail --mode huge"), &mut s),
            Submission::Local
        ));
        assert_eq!(c.detail, Detail::Line);
        let said = s.0.join("\n");
        assert!(said.contains("all, line, one"), "{said}");
    }

    #[test]
    fn client_commands_never_reach_the_wire() {
        let d = tempfile::tempdir().unwrap();
        let cat = Catalog::new();
        cat.set_sticky_names(&[]);
        let mut c = client_with(cat, Sticky::load(d.path()), vec!["main"]);
        let mut s = VecSink(Vec::new());
        assert!(matches!(
            c.command(
                argv("set_persistant_cmd_argument --name session --value main"),
                &mut s
            ),
            Submission::Local
        ));
        assert_eq!(
            c.sticky.pairs(),
            vec![("session".to_string(), "main".to_string())]
        );
        assert!(matches!(
            c.command(argv("del_persistant_cmd_argument --name session"), &mut s),
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
        cat.set_server_commands(vec![protocol::CommandSpec {
            name: "del_session".into(),
            summary: "s".into(),
            args: vec![protocol::ArgSpec {
                name: "session".into(),
                summary: "s".into(),
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

    /// Naming a session is what makes it the one this client watches.
    #[test]
    fn set_session_leaves_a_sticky_session_and_its_filter() {
        let d = tempfile::tempdir().unwrap();
        let cat = Catalog::new();
        cat.set_server_commands(vec![protocol::CommandSpec {
            name: "set_session".into(),
            summary: "s".into(),
            args: vec![protocol::ArgSpec {
                name: "session".into(),
                summary: "s".into(),
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
        assert_eq!(c.filters.get("session"), Some("main"));
    }

    /// A refused `set_session` must not leave the client watching a session that
    /// was never created.
    #[test]
    fn a_refused_set_session_leaves_the_client_where_it_was() {
        let d = tempfile::tempdir().unwrap();
        let cat = Catalog::new();
        cat.set_server_commands(vec![protocol::CommandSpec {
            name: "set_session".into(),
            summary: "s".into(),
            args: vec![protocol::ArgSpec {
                name: "session".into(),
                summary: "s".into(),
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
        cat.set_server_commands(vec![protocol::CommandSpec {
            name: "del_session".into(),
            summary: "s".into(),
            args: vec![protocol::ArgSpec {
                name: "session".into(),
                summary: "s".into(),
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
        assert_eq!(c.filters.get("session"), None);
    }

    /// A field no event carries would filter nothing; say so instead of
    /// storing a filter that quietly does nothing.
    #[test]
    fn set_event_filter_refuses_a_field_no_event_has() {
        let d = tempfile::tempdir().unwrap();
        let mut c = client_with(Catalog::new(), Sticky::load(d.path()), vec!["main"]);
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
        cat.set_server_commands(vec![protocol::CommandSpec {
            name: "del_session".into(),
            summary: "s".into(),
            args: vec![protocol::ArgSpec {
                name: "name".into(),
                summary: "s".into(),
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
