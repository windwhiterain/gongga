//! `gongga-cli`: the command layer.
//!
//! This crate owns the command surface: it knows the catalog the server
//! publishes, keeps the sticky arguments, and turns one command's tokens into
//! either a local action or a message on the wire.
//!
//! It does not own the *input line*. Whether a line is a `/command` or prose for
//! the agent is the interactive front-end's decision ([`Client::command`] and
//! [`Client::message`] are the two things that decision can ask for), so the
//! one-shot path -- `gongga send_message --message hi` -- never has to guess: its
//! argv is already tokens.
//!
//! It knows nothing about terminals. That is the point of the split: anything
//! that can present [`Sink`] events can drive it, and the interactive client is
//! one such thing (the `gongga` binary in `crates/tui`). Nothing here names a
//! rendering crate, so the command layer builds into a script, a test, or a UI
//! without dragging a terminal along.
//!
//! Wire encoding stays here too: [`Link`] is the only thing that speaks the
//! protocol, which is what lets a front-end hold a link without learning what a
//! frame is.

pub mod catalog;
pub mod commands;
pub mod complete;

pub use crate::catalog::Catalog;
pub use crate::commands::Sticky;
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

use crate::commands::{parse_args, render_args};

const DEFAULT_SERVER: &str = "127.0.0.1:9700";
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(5);

// The parts of the command line that say *where* to connect.
//
// Separate from `Args` because a front-end has the same three flags but no
// commands: it flattens this in and gets the resolution rules (flag, then
// environment, then config.toml) from one definition.
//
// Deliberately *not* a doc comment: clap folds a flattened struct's `///` text
// into the parent command's about line, which would replace the program's own
// description with this paragraph.
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
    /// Where to dial and with which key.
    ///
    /// Resolved once per process: a running client does not re-read
    /// config.toml between retries, because a file edited mid-outage would make
    /// two attempts of the same reconnect mean two different servers.
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
    /// Rejects a global flag written after the command name.
    ///
    /// clap hands an external subcommand every token after it, so `gongga
    /// send_message --server 1.2.3.4:1` would take `--server` as one of the
    /// command's own arguments and then quietly dial the default server.
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
    /// Anything but a flag is the command itself, because what the client can
    /// run is decided by the server and only known after the handshake: there
    /// is no fixed list of subcommands to declare here.
    ///
    /// `gongga set_session --name main`. One command per process, the answer on
    /// stdout as JSON lines, then exit — this is how a script (or the model in
    /// a `bash` tool) drives the harness.
    #[command(external_subcommand)]
    Cmd(Vec<String>),
}

/// Parses the command line, the one way both binaries do it.
pub fn parse_argv() -> Args {
    Args::parse()
}

// ---------------------------------------------------------------------------
// Where rendered output goes
// ---------------------------------------------------------------------------

/// What a rendered line is, which decides how it is presented.
///
/// This is the command layer's own vocabulary: the layer that reads events is
/// the one that knows a line came from a tool call or a failure, so it says so.
/// A front-end turns that into a colour; it does not decide what it is looking
/// at.
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

/// Everything the client can ask a front-end to show.
///
/// The interactive client (`crates/tui`) implements it with styled lines;
/// one-shot mode emits the events themselves as JSON and renders nothing.
/// Neither implementation knows what produced the text.
pub trait Sink {
    fn line(&mut self, kind: LineKind, text: &str);
    fn stream(&mut self, key: &str, kind: LineKind, prefix: &str, text: &str, done: bool);
    fn status(&mut self, text: &str);
    fn footer(&mut self, text: &str);
}

/// The one-shot mode's sink: it prints nothing, but it remembers what went
/// wrong.
///
/// What a script gets is the event stream itself, one JSON object per line,
/// emitted where the events arrive. Anything the client renders for humans
/// (banners, status lines, streamed text) belongs to the TUI only — except
/// errors, which are the one thing a caller with no screen still needs, since
/// a line rejected before it ever reached the server produces no event at all.
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
    /// Nothing was sent because there is no link. Distinct from a rejection:
    /// the line is valid, its turn had simply not come, so the front-end puts
    /// it back in the prompt instead of treating it as a mistake.
    Offline,
}

// ---------------------------------------------------------------------------
// Client state
// ---------------------------------------------------------------------------

pub struct Client {
    catalog: Catalog,
    sticky: Sticky,
    sessions: Vec<SessionSummary>,
    model: String,
    next_id: u64,
    /// Whether a link is open right now.
    ///
    /// Everything else the client knows about the server — the catalog, the
    /// sessions, the model — is what the last greeting said. While this is
    /// false that knowledge is stale by definition, and the status line says so
    /// rather than showing a model number as if the server were still there.
    connected: bool,
    /// Failed dials since the link went down, so silence is legible.
    attempts: u32,
}

/// What the operator typed, for the purpose of echoing it back into the log.
///
/// Both halves of the grammar reach the same code; the only difference the
/// operator should see is that a command looks like a command (`/set_session
/// --session main`) and a message looks like what they wrote. The wire never
/// sees this distinction: a message is `send_message --message <text>`.
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
        Self {
            catalog,
            sticky,
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

    /// Feeds one datagram from the link into the client.
    ///
    /// Decoding lives here so a front-end never has to name a `ServerMessage`:
    /// it passes the bytes it got from [`Link::recv`] and reads the result off
    /// the screen.
    pub fn handle_bytes(&mut self, bytes: &[u8], sink: &mut dyn Sink) -> anyhow::Result<()> {
        let msg = decode_server(bytes)?;
        self.handle(msg, sink);
        Ok(())
    }

    /// The completion source for the input line.
    ///
    /// The catalog is the same object, shared rather than copied: when a
    /// greeting replaces the server's half, the popup sees it without the
    /// front-end being told anything.
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
                // A greeting *is* the recovery. There is no separate resync
                // path: the same message that starts a session also hands back
                // the catalog and the session list, so a client that redials
                // after a restart comes up knowing exactly what a fresh one
                // does. That is the whole reason reconnecting is cheap.
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
        match event {
            Event::Cmd { name, args, .. } => {
                let rendered = render_args(&args);
                sink.line(LineKind::System, &format!("$ {name}{rendered}"));
            }
            Event::CmdResult { message, ok, .. } => {
                if message.is_empty() {
                    return;
                }
                sink.line(
                    if ok {
                        LineKind::Result
                    } else {
                        LineKind::Error
                    },
                    &message,
                );
            }
            Event::Response(e) => self.handle_response(e, sink),
            Event::ToolAnswer(e) => self.handle_tool_answer(e, sink),
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
                let label = match status {
                    SessionStatus::Idle => "idle",
                    SessionStatus::Running => "running",
                    SessionStatus::Error => "error",
                };
                let suffix = detail.map(|d| format!(" ({d})")).unwrap_or_default();
                sink.line(LineKind::System, &format!("[{session}] {label}{suffix}"));
                if let Some(s) = self.sessions.iter_mut().find(|s| s.name == session) {
                    s.status = status;
                }
                self.refresh(sink);
            }
            Event::Log { level, message } => {
                let kind = match level {
                    LogLevel::Debug | LogLevel::Info => LineKind::System,
                    LogLevel::Warn | LogLevel::Error => LineKind::Error,
                };
                sink.line(kind, &message);
            }
        }
    }

    fn handle_response(&self, e: ResponseEvent, sink: &mut dyn Sink) {
        let key = format!("r:{}:{}:{:?}", e.session, e.turn, e.part);
        let (kind, prefix) = match e.part {
            ResponsePart::Reasoning => (LineKind::Reasoning, "thinking: "),
            ResponsePart::ToolCall => (LineKind::ToolCall, "tool: "),
            ResponsePart::Output => (LineKind::Assistant, ""),
        };
        sink.stream(&key, kind, prefix, &e.text, e.done);
    }

    fn handle_tool_answer(&self, e: ToolAnswerEvent, sink: &mut dyn Sink) {
        let key = format!("t:{}:{:?}", e.call_id, e.stream);
        let kind = match e.stream {
            ToolStream::Stdout => LineKind::ToolOutput,
            ToolStream::Stderr => LineKind::ToolError,
        };
        sink.stream(&key, kind, "", &e.text, e.done);
    }

    fn session_names(&self) -> Vec<String> {
        self.sessions.iter().map(|s| s.name.clone()).collect()
    }

    /// Keeps the completer's `--name` candidates in step with the sticky set.
    pub fn sync_sticky(&self) {
        self.catalog.set_sticky_names(&self.sticky.names());
    }

    /// The status line.
    ///
    /// Deliberately has no "current session" in it. The only thing that ever
    /// stood there was the sticky `session` argument, and printing it turned a
    /// persistant argument into something that looked like a mode the UI was
    /// in. What is left are facts about the server: its model and how many
    /// sessions exist — and, when there is no link, that there is no link,
    /// because a model name with no server behind it is a lie the operator
    /// cannot see through.
    pub fn refresh(&self, sink: &mut dyn Sink) {
        let facts = if self.connected {
            format!("model {} | {} session(s)", self.model, self.sessions.len())
        } else if self.attempts == 0 {
            "offline — connecting…".to_string()
        } else {
            format!("offline — reconnecting (attempt {})", self.attempts)
        };
        sink.status(&facts);
        let sticky = self
            .sticky
            .pairs()
            .iter()
            .map(|(k, v)| format!("{k}={v}"))
            .collect::<Vec<_>>()
            .join("  ");
        sink.footer(&if sticky.is_empty() {
            "sticky: (none)".to_string()
        } else {
            format!("sticky: {sticky}")
        });
    }

    /// Records that there is no link, and how many dials have failed since.
    pub fn set_offline(&mut self, attempts: u32, sink: &mut dyn Sink) {
        self.connected = false;
        self.attempts = attempts;
        self.refresh(sink);
    }

    /// Runs one command, from argv that is already split into tokens.
    ///
    /// This is the whole interface of the one-shot path
    /// (`gongga send_message --message hi`). There is no grammar here on
    /// purpose: a command line arrives tokenised by the shell that started the
    /// process, so there is nothing left to decide. `/name` is the *interactive*
    /// input line's syntax and prose is not a command; splitting a string back
    /// into tokens only to re-derive an intention would let the two paths
    /// disagree, and that disagreement is exactly what made `$GONGGA_CLI cancel`
    /// -- the form the agent is told to use -- send the word "cancel" to the
    /// model as a chat message.
    pub fn command(&mut self, tokens: Vec<String>, sink: &mut dyn Sink) -> Submission {
        self.submit_tokens(tokens, sink, Echo::Command)
    }

    /// Sends one line of prose to the agent as a message.
    ///
    /// The interactive input line decides that a line without `/` is prose; this
    /// is what it asks for then. Assembled as tokens, never as a line: whatever
    /// the operator typed -- quotes, `--`, backslashes, a whole paragraph -- is
    /// one argument, and no parsing rule can turn part of it into a flag.
    pub fn message(&mut self, text: &str, sink: &mut dyn Sink) -> Submission {
        let tokens = vec![
            "send_message".to_string(),
            "--message".to_string(),
            text.to_string(),
        ];
        self.submit_tokens(tokens, sink, Echo::Message(text))
    }

    /// The one path both entries take, once the caller has decided what the
    /// line meant.
    ///
    /// `echo` is only about what the operator sees: a command is echoed as the
    /// input line showed it, a message as the prose it was. What goes on the
    /// wire has the same shape either way.
    ///
    /// Everything below is about *commands*: the catalog, sticky arguments,
    /// required arguments, the wire. That a line without `/` is prose was
    /// decided before this, by whoever had an input box.
    fn submit_tokens(
        &mut self,
        tokens: Vec<String>,
        sink: &mut dyn Sink,
        echo: Echo<'_>,
    ) -> Submission {
        let Some(head) = tokens.first().cloned() else {
            return Submission::Rejected;
        };

        // Offline, the server's half of the catalog is whatever the last
        // greeting said — possibly nothing at all, on a client started before
        // its server. Falling through to "unknown command" would then blame the
        // operator for the server being down, so say what is really wrong.
        // Client commands are unaffected: they never leave this process.
        if !self.connected && !self.catalog.is_client_command(&head) {
            sink.line(
                LineKind::Error,
                &format!("{head}: not connected to the server, nothing was sent"),
            );
            return Submission::Offline;
        }

        let Some(spec) = self.catalog.command(&head) else {
            // A command line is not an input line: here a command is just its
            // name. Two shapes still arrive as something else --
            //
            //   `/name ...`          the interactive input line's syntax, carried
            //                        onto a command line by a script or a model
            //   `C:/.../Git/name ...` the same thing after Git Bash rewrote the
            //                        leading `/` into a path, which happens
            //                        before this program is even started
            //
            // -- and both are best answered with the line that was meant.
            // "unknown command" over a path nobody typed says nothing about the
            // mistake, and that mistake is easy to make: the agent is told to
            // drive the harness with `$GONGGA_CLI <command>`.
            //
            // The last separator is what finds the name in both shapes. Split by
            // hand rather than with `Path`, which does not call `\\` a separator
            // on every platform.
            if head.contains('/') || head.contains('\\') {
                let tail = match head.rfind(['/', '\\']) {
                    Some(at) => &head[at + 1..],
                    None => head.as_str(),
                };
                let name = tail.split_whitespace().next().unwrap_or_default();
                // Whatever followed the name, in either shape: inside the one
                // argument when the whole line was quoted, or in the remaining
                // tokens when it was real argv.
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
        // A flag the command does not take is a typo, not decoration. Passing it
        // through used to be harmless-looking and wrong: after the session
        // argument was renamed, `--name x` filled nothing, a sticky `session`
        // filled the real one, and the command quietly applied to a session the
        // caller had not named. Checked before `fill` so only what was typed is
        // judged -- sticky arguments never introduce unknown names.
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
        Submission::Send(ClientMessage::Command {
            id: self.next_id(),
            name: head,
            args,
        })
    }

    fn run_client_command(&mut self, name: &str, tokens: &[String], sink: &mut dyn Sink) {
        let args = match parse_args(tokens) {
            Ok(a) => a,
            Err(e) => {
                sink.line(LineKind::Error, &e.to_string());
                return;
            }
        };
        let Some(arg_name) = args.get("name").cloned() else {
            sink.line(LineKind::Error, &format!("{name} requires --name"));
            return;
        };
        match name {
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

// ---------------------------------------------------------------------------
// Front-ends
// ---------------------------------------------------------------------------

/// One open link: the two halves of the channel, kept together because a link
/// is either wholly there or wholly gone.
///
/// A front-end holds one of these but never sees a frame. Encoding and decoding
/// stay behind these methods, so "what does the wire look like" remains a
/// question only this crate answers.
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

    /// Best-effort goodbye, so the server can drop this connection now instead
    /// of waiting out its idle timeout. Worth nothing if it fails: the caller is
    /// already closing.
    pub async fn goodbye(&self) {
        let _ = self.send(ClientMessage::Bye).await;
    }
}

/// Headless: one command in, its result out.
///
/// Runs one command against the server, prints the events, exits.
///
/// `gongga send_message --message hi --session main`. One command per process is
/// the whole interface: there is no `exec` to wrap it in, and nothing to
/// configure about the wait — the command returns when the server answers, which
/// for `send_message` is the moment the message is accepted, not the moment the
/// turn finishes.
///
/// The argv is taken as it arrived. It is not turned back into a line first,
/// because the line grammar is the interactive front-end's: here a command is
/// just its name and its arguments, and there is no second meaning to fall into.
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
        // A client-side command already did its work; nothing goes on the wire.
        Submission::Local => return Ok(()),
        // Rejected before it reached the server, so there is no event to print.
        // The reason still has to come out: "did not produce a command" would
        // leave the caller guessing whether the name or an argument was wrong.
        Submission::Rejected => {
            let why = printer
                .take_error()
                .unwrap_or_else(|| "not a command".to_string());
            anyhow::bail!("{why}");
        }
        // Cannot happen here — this path only runs after a successful dial —
        // but a silent `todo!()` would be worse than saying so.
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

    // No deadline: what is being waited for is one command's acknowledgement,
    // not the work it may start. `send_message` returns as soon as the server
    // has taken the message, and the turn runs afterwards. A server that stops
    // answering drops the connection at its own idle timeout, which ends this
    // wait with an error of its own.
    loop {
        let Some(bytes) = link.recv().await else {
            anyhow::bail!("server disconnected");
        };
        let ServerMessage::Event(event) = decode_server(&bytes)? else {
            continue;
        };
        // The event *is* the output. JSON lines survive `jq`, a model reading
        // them, and a pipe; rendered text survives none of those.
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

// ---------------------------------------------------------------------------
// Connection
// ---------------------------------------------------------------------------

/// Where to dial and with which key.
///
/// Resolved once per process: a running client does not re-read config.toml
/// between retries, because a file edited mid-outage would make two attempts of
/// the same reconnect mean two different servers.
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
    /// Asking again cannot help: the key is wrong, the address is malformed.
    /// Retrying would only hide the reason behind an endless "reconnecting".
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

/// Dials, authenticates, and waits for the greeting.
///
/// Every call binds a new local port on purpose. The server routes datagrams by
/// peer address, so a link that died and came back on the same address would be
/// handed to the connection the server still remembers — the dead one. A fresh
/// port makes the SYN unambiguously a new connection, which is also what the
/// server's own demultiplexer expects.
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

    // The greeting is the first thing the server sends, so waiting for it is
    // both the handshake confirmation and the command catalog download.
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

/// The server's greeting, already decoded.
///
/// Opaque on purpose: the greeting *is* a `ServerMessage`, and keeping it out of
/// the signature is what lets a front-end dial without linking against the
/// protocol. It is applied later rather than inside [`dial`] so that dialling
/// borrows nothing but its endpoint -- which is what lets the interactive loop
/// keep dialling while it still owns the screen.
pub struct Greeting(ServerMessage);

impl Greeting {
    /// Brings a client up to date: catalog, session list, model.
    ///
    /// This *is* the resync, and there is no separate path -- the same message
    /// that starts a session also hands back everything a fresh client needs,
    /// which is the whole reason reconnecting is cheap.
    pub fn apply(self, client: &mut Client, sink: &mut dyn Sink) {
        client.handle(self.0, sink);
    }
}

/// The client's own state, built without touching the network.
///
/// The catalog starts with only the client's half; the server's half arrives
/// with the greeting, which is why a client can exist before its server does.
pub fn build_client(args: &Args) -> Client {
    let home = home_dir(args.conn.home.as_deref());
    let catalog = Catalog::new();
    let sticky = Sticky::load(&home);
    catalog.set_sticky_names(&sticky.names());
    Client::new(catalog, sticky)
}

/// Connects and returns a client that already knows the server's catalog.
///
/// One-shot mode only: there is no reconnect here, because one command that
/// cannot reach a server has nothing to wait for. An interactive client dials
/// from inside its own loop instead, so it can outlive any number of restarts.
async fn connect(args: &Args) -> anyhow::Result<(Client, Link)> {
    // One-shot mode's caller is usually a script or the model, so the one thing
    // worth adding to the transport's own words is the usual cause.
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

/// The process environment, as far as these lookups are concerned.
///
/// Injected rather than read directly so the precedence rules can be tested
/// without mutating global state. That is not tidiness: the harness exports
/// `GONGGA_SERVER` into every tool call's shell (it is how the agent reaches
/// its own server), so a test that reads the real environment passes or fails
/// depending on who launched it.
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

    fn home_with(text: &str) -> tempfile::TempDir {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(d.path().join("config.toml"), text).unwrap();
        d
    }

    /// A command as argv, the way the one-shot path receives it.
    ///
    /// The command layer takes tokens, not a line, so this is what the tests
    /// below hand it -- splitting here would be the thing being avoided.
    fn argv(line: &str) -> Vec<String> {
        shell_words::split(line).expect("test argv parses")
    }

    /// `gongga send_message --message "two words"` has to reach the server as
    /// one argument, not two. Nothing splits or rejoins the argv, so the only
    /// thing that could go wrong is the tokenizer inside the command layer.
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

    /// `/` is the interactive input line's syntax. On a command line it is not
    /// a command name, and answering "unknown command `"/send_message"`" would
    /// hide the one fact worth knowing -- so the rejection spells the line out.
    ///
    /// This is not hypothetical: the agent is told to drive the harness with
    /// `$GONGGA_CLI <command>`, carried the TUI's `/` onto it, and on Windows
    /// Git Bash turned the `/word` into a `C:/.../` path before the client ever
    /// saw it, so the command quietly became a chat message.
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

    /// The same mistake *after* the shell has rewritten it.
    ///
    /// Git Bash turns `/get_messages` into a path before the program is even
    /// started, so what arrives is a path ending in the command's name -- with
    /// the rest of the line attached when it was quoted as one argument. Both
    /// shapes have to come back as the command line that was meant, or the
    /// operator is left with "unknown command" over a path they never typed.
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

    /// The layering this crate exists for. A command layer that pulled in the
    /// front-end would drag a terminal into every script and every test that
    /// only wants to send a command, and the cycle would come back the moment
    /// the front-end needed a command name.
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
        // And it ships no program of its own: the binary is the composition
        // root, which lives where both halves are available.
        // The *comment* in the manifest says the same thing, so match a real
        // section header rather than the substring.
        assert!(
            !manifest.lines().any(|l| l.trim() == "[[bin]]"),
            "{manifest}"
        );
    }

    /// A shell with nothing set. The real one is not empty -- the harness
    /// exports GONGGA_SERVER/GONGGA_KEY so the agent can drive itself -- so
    /// every precedence test states its environment explicitly.
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

    /// The harness sets both of these for every tool call, so this is the
    /// ordinary case, not an exotic one: the shell beats the config file.
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

    /// Drives a command without a terminal or a socket.
    ///
    /// The client is marked connected: these tests are about what a command
    /// means, and an offline client refuses every server command before it gets
    /// that far (see `submit_tokens`), which would make every one of them test
    /// the same thing.
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

    /// One-shot mode renders nothing, but a rejected line has no event to fall
    /// back on, so the reason has to survive somewhere the caller can read it.
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

    /// The input line is a command, always.
    ///
    /// The status line states facts about the server, not which session the
    /// front-end considers itself to be in.
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

    /// A flag the command does not take is refused, and the refusal names the
    /// ones it does take. This is the guard against a renamed argument: the old
    /// spelling must not be accepted and silently ignored.
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
        // The pre-rename spelling is exactly the case that bit: it used to be
        // passed through, and a sticky `session` made it look like it worked.
        assert!(matches!(
            c.command(argv("del_session --name main"), &mut s),
            Submission::Rejected
        ));
        let said = s.0.join("\n");
        assert!(said.contains("has no argument --name"), "{said}");
        assert!(said.contains("--session"), "{said}");
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
