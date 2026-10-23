//! `gongga`: the terminal client.
//!
//! The client owns the command surface — it knows the catalog the server
//! publishes, keeps the sticky arguments, and turns an input line into either a
//! local action or a message on the wire. The TUI is driven through [`Sink`] and
//! never learns what a command is.

mod catalog;
mod commands;

use std::future::Future;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::pin::Pin;
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
use tui::LineKind;

use crate::catalog::Catalog;
use crate::commands::{Sticky, parse_args, render_args};

const DEFAULT_SERVER: &str = "127.0.0.1:9700";
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(5);
/// How long the interactive client waits before redialling, doubling up to the
/// cap. The first retry is fast enough to be invisible across a `dev.sh restart`
/// and the cap keeps a server that is down for an hour from spinning.
const RECONNECT_MIN: Duration = Duration::from_millis(250);
const RECONNECT_MAX: Duration = Duration::from_secs(5);

#[derive(Debug, Parser)]
#[command(name = "gongga", about = "Gongga harness terminal client", version)]
struct Args {
    /// Server address, e.g. `127.0.0.1:9700`.
    #[arg(long, global = true)]
    server: Option<String>,
    /// Harness home holding config.toml and cli.toml. Defaults to `~/.gongga`.
    #[arg(long, global = true)]
    home: Option<PathBuf>,
    /// Connection key. Defaults to `[server] key` in ~/.gongga/config.toml.
    #[arg(long, global = true)]
    key: Option<String>,
    #[command(subcommand)]
    command: Option<Sub>,
}

#[derive(Debug, clap::Subcommand)]
enum Sub {
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

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    match &args.command {
        None => interactive(&args).await,
        // `gongga set_session --name main`, i.e. the command *is* the
        // subcommand.
        Some(Sub::Cmd(command)) => {
            // clap hands an external subcommand every token after it, so a
            // global flag written there would be taken as one of the command's
            // own arguments and the client would quietly dial the default
            // server. Refuse it instead of connecting somewhere else than asked.
            if let Some(flag) = command.iter().find(|t| {
                ["--server", "--home", "--key"]
                    .iter()
                    .any(|f| *t == f || t.starts_with(&format!("{f}=")))
            }) {
                anyhow::bail!(
                    "{flag} has to come before the command: \
                     `gongga {flag} <value> {} ...`",
                    command[0]
                );
            }
            run(&args, command).await
        }
    }
}

// ---------------------------------------------------------------------------
// Where rendered output goes
// ---------------------------------------------------------------------------

/// Everything the client can ask a front-end to show.
///
/// The TUI implements it with styled lines; one-shot mode emits the events
/// themselves as JSON and renders nothing. Neither implementation knows what
/// produced the text.
trait Sink {
    fn line(&mut self, kind: LineKind, text: &str);
    fn stream(&mut self, key: &str, kind: LineKind, prefix: &str, text: &str, done: bool);
    fn status(&mut self, text: &str);
    fn footer(&mut self, text: &str);
}

struct TuiSink<'a>(&'a mut tui::Tui);

impl Sink for TuiSink<'_> {
    fn line(&mut self, kind: LineKind, text: &str) {
        self.0.push(kind, text);
    }

    fn stream(&mut self, key: &str, kind: LineKind, prefix: &str, text: &str, done: bool) {
        self.0.stream(key, kind, prefix, text, done);
    }

    fn status(&mut self, text: &str) {
        self.0.set_status(text);
    }

    fn footer(&mut self, text: &str) {
        self.0.set_footer(text);
    }
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
enum Submission {
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

struct Client {
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

    fn handle(&mut self, msg: ServerMessage, sink: &mut dyn Sink) {
        match msg {
            ServerMessage::Hello { server, commands, sessions, .. } => {
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
                sink.line(if ok { LineKind::Result } else { LineKind::Error }, &message);
            }
            Event::Response(e) => self.handle_response(e, sink),
            Event::ToolAnswer(e) => self.handle_tool_answer(e, sink),
            Event::Sessions { sessions } => {
                self.sessions = sessions;
                self.catalog.set_sessions(self.session_names());
                self.refresh(sink);
            }
            Event::Status { session, status, detail } => {
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
    fn sync_sticky(&self) {
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
    fn refresh(&self, sink: &mut dyn Sink) {
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
    fn set_offline(&mut self, attempts: u32, sink: &mut dyn Sink) {
        self.connected = false;
        self.attempts = attempts;
        self.refresh(sink);
    }

    /// Turns one input line into either a local action or a message to send.
    ///
    /// Command-only, in both front-ends. There used to be a shorthand where
    /// bare text meant `send_message` to the sticky session; that made the
    /// input line mean two different things depending on invisible state, and
    /// it is the reason the UI appeared to have a current session at all. One
    /// grammar now: every line is a command, and a command names its session
    /// (directly, or through a persistant argument the user set explicitly).
    fn submit(&mut self, raw: &str, sink: &mut dyn Sink) -> Submission {
        let tokens = match shell_words::split(raw) {
            Ok(t) => t,
            Err(e) => {
                sink.line(LineKind::Error, &format!("unparsable input: {e}"));
                return Submission::Rejected;
            }
        };
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
            sink.line(
                LineKind::Error,
                &format!(
                    "unknown command {head:?}: every line is a command; to talk, \
                     `send_message --message <text> --session <name>`"
                ),
            );
            return Submission::Rejected;
        };

        if self.catalog.is_client_command(&head) {
            sink.line(LineKind::System, &format!("$ {raw}"));
            self.run_client_command(&head, &tokens[1..], sink);
            return Submission::Local;
        }

        sink.line(LineKind::System, &format!("$ {raw}"));
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
                    sink.line(LineKind::Error, "set_persistant_cmd_argument requires --value");
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
            other => sink.line(LineKind::Error, &format!("unknown client command {other:?}")),
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
struct Link {
    sink: protocol::ChannelSink,
    stream: protocol::ChannelStream,
}

/// What the loop is waiting for this time round.
enum Step {
    /// The operator did something (or the terminal went away: `None`).
    Input(Option<tui::UiEvent>),
    /// A message arrived, or `None` when the link ended.
    Wire(Option<Vec<u8>>),
    /// The retry backoff expired: start dialling.
    Dial,
    /// A dial in flight finished.
    Dialed(Result<(Link, ServerMessage), DialFailure>),
}

/// A dial kept alive between loop iterations.
///
/// Boxed rather than awaited inside the branch: an unreachable server takes the
/// whole handshake timeout to give up, and waiting for it inside the loop would
/// freeze the keyboard for five seconds at a stretch — the TUI would be
/// unresponsive exactly while the server is down, which is when the operator is
/// most likely to be pressing Ctrl+Q.
type Dialing = Pin<Box<dyn Future<Output = Result<(Link, ServerMessage), DialFailure>>>>;

/// The interactive front-end.
///
/// The window and the link are separate things. Nothing here makes the server's
/// presence a condition for the client to exist: the TUI opens first, dials from
/// inside the loop, and when the link dies it goes back to dialling instead of
/// taking the window down with it. A restart therefore looks like two lines in
/// the log — `connection lost`, then `connected` — with the input line, the
/// history and the scroll position exactly where they were.
async fn interactive(args: &Args) -> anyhow::Result<()> {
    // Resolved before the terminal is taken over: a bad address or a missing
    // key is not fixed by retrying, and an error is far easier to read on the
    // normal screen than after `restore()` flashed the alternate screen away.
    let endpoint = endpoint(args)?;

    let mut client = build_client(args);
    let mut ui = tui::Tui::new(Arc::new(client.catalog.clone()))?;
    client.set_offline(0, &mut TuiSink(&mut ui));

    let mut link: Option<Link> = None;
    let mut dialing: Option<Dialing> = None;
    let mut next_try = tokio::time::Instant::now();
    let mut backoff = RECONNECT_MIN;
    let mut attempts: u32 = 0;

    let result = loop {
        // Every arm keeps the keyboard branch, so no state — connected, waiting,
        // or mid-handshake — can make the window stop answering keys.
        let step = if let Some(open) = link.as_mut() {
            tokio::select! {
                event = ui.next_event() => Step::Input(event),
                bytes = open.stream.recv() => Step::Wire(bytes),
            }
        } else if let Some(pending) = dialing.as_mut() {
            tokio::select! {
                event = ui.next_event() => Step::Input(event),
                outcome = pending.as_mut() => Step::Dialed(outcome),
            }
        } else {
            tokio::select! {
                event = ui.next_event() => Step::Input(event),
                _ = tokio::time::sleep_until(next_try) => Step::Dial,
            }
        };

        match step {
            Step::Input(None) => break Ok(()),
            Step::Input(Some(tui::UiEvent::Quit)) => break Ok(()),
            Step::Input(Some(tui::UiEvent::Submit(line))) => {
                match client.submit(&line, &mut TuiSink(&mut ui)) {
                    Submission::Local | Submission::Rejected => {}
                    // Nothing left this process, so the text is still the
                    // operator's. Put it back rather than log it away.
                    Submission::Offline => ui.restore_input(&line),
                    Submission::Send(msg) => match link.as_ref() {
                        Some(open) => match encode_client(&msg) {
                            Ok(bytes) => {
                                if let Err(e) = open.sink.send(bytes).await {
                                    // The channel task is gone but its
                                    // half-open end had not been noticed yet.
                                    // This message may or may not have arrived,
                                    // so it is reported, not silently re-queued.
                                    TuiSink(&mut ui).line(
                                        LineKind::Error,
                                        &format!(
                                            "connection dropped while sending ({e}); \
                                             that line may not have arrived"
                                        ),
                                    );
                                    link = None;
                                    backoff = RECONNECT_MIN;
                                    attempts = 0;
                                    next_try = tokio::time::Instant::now();
                                    client.set_offline(0, &mut TuiSink(&mut ui));
                                }
                            }
                            Err(e) => {
                                TuiSink(&mut ui)
                                    .line(LineKind::Error, &format!("encoding failed: {e}"));
                            }
                        },
                        // Raced with the link going down between `submit` and
                        // here. Nothing was sent, so the line goes back.
                        None => ui.restore_input(&line),
                    },
                }
            }
            Step::Wire(Some(bytes)) => match decode_server(&bytes) {
                Ok(msg) => client.handle(msg, &mut TuiSink(&mut ui)),
                Err(e) => TuiSink(&mut ui).line(LineKind::Error, &format!("bad message: {e}")),
            },
            Step::Wire(None) => {
                link = None;
                backoff = RECONNECT_MIN;
                attempts = 0;
                next_try = tokio::time::Instant::now();
                client.set_offline(0, &mut TuiSink(&mut ui));
                TuiSink(&mut ui).line(LineKind::System, "server connection lost — reconnecting");
            }
            Step::Dial => {
                attempts += 1;
                dialing = Some(Box::pin(dial(endpoint.clone())));
            }
            Step::Dialed(outcome) => {
                // Finished either way: clear it so a later outage cannot poll a
                // dead future, which would panic.
                drop(dialing.take());
                match outcome {
                    Ok((open, greeting)) => {
                        link = Some(open);
                        backoff = RECONNECT_MIN;
                        // The greeting is the resync: catalog, sessions, model.
                        client.handle(greeting, &mut TuiSink(&mut ui));
                    }
                    Err(failure) if failure.is_fatal() => break Err(failure.into_error()),
                    Err(failure) => {
                        client.set_offline(attempts, &mut TuiSink(&mut ui));
                        // Once per outage: the status line carries "attempt N",
                        // so repeating this every few hundred milliseconds
                        // would bury the log without adding information.
                        if attempts == 1 {
                            TuiSink(&mut ui).line(
                                LineKind::Error,
                                &format!(
                                    "{} — retrying in the background, Ctrl+Q still leaves",
                                    failure.message()
                                ),
                            );
                        }
                        backoff = (backoff * 2).min(RECONNECT_MAX);
                        next_try = tokio::time::Instant::now() + backoff;
                    }
                }
            }
        }
    };

    // Best-effort goodbye: the server drops this connection now instead of
    // waiting out its idle timeout. Worth nothing if it fails — the window is
    // already closing.
    if let Some(open) = link.take() {
        if let Ok(bytes) = encode_client(&ClientMessage::Bye) {
            let _ = open.sink.send(bytes).await;
        }
    }

    ui.restore()?;
    result
}

/// Headless: one command in, its result out.
/// Turns the argv of one command into the line the client parses.
///
/// A single argument is already a whole line -- `exec "send_message ..."`, the
/// shape scripts have always used -- so it passes through verbatim. Anything
/// else is argv, and joining it under quoting rules is what keeps an argument
/// containing a space from arriving as two.
fn command_line(command: &[String]) -> String {
    match command {
        [one] => one.clone(),
        many => shell_words::join(many),
    }
}

/// Runs one command against the server, prints the events, exits.
///
/// `gongga send_message --message hi --session main`, or the same line quoted
/// as one argument. One command per process is the whole interface: there is no
/// `exec` to wrap it in, and nothing to configure about the wait — the command
/// returns when the server answers, which for `send_message` is the moment the
/// message is accepted, not the moment the turn finishes.
async fn run(args: &Args, command: &[String]) -> anyhow::Result<()> {
    let line = command_line(command);
    let (mut client, sink, mut stream) = connect(args).await?;
    let result = run_once(&mut client, &sink, &mut stream, &line).await;
    // A best-effort goodbye lets the server tear the connection down now
    // instead of waiting out the idle timeout.
    let _ = sink.send(encode_client(&ClientMessage::Bye)?).await;
    result
}

async fn run_once(
    client: &mut Client,
    sink: &protocol::ChannelSink,
    stream: &mut protocol::ChannelStream,
    command: &str,
) -> anyhow::Result<()> {
    let mut printer = NullSink::default();
    let msg = match client.submit(command, &mut printer) {
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
    sink.send(encode_client(&msg)?)
        .await
        .map_err(|e| anyhow!("send failed: {e}"))?;

    // No deadline: what is being waited for is one command's acknowledgement,
    // not the work it may start. `send_message` returns as soon as the server
    // has taken the message, and the turn runs afterwards. A server that stops
    // answering drops the connection at its own idle timeout, which ends this
    // wait with an error of its own.
    loop {
        let Some(bytes) = stream.recv().await else {
            anyhow::bail!("server disconnected");
        };
        let ServerMessage::Event(event) = decode_server(&bytes)? else {
            continue;
        };
        // The event *is* the output. JSON lines survive `jq`, a model reading
        // them, and a pipe; rendered text survives none of those.
        println!("{}", serde_json::to_string(&event)?);
        if let Event::CmdResult { id: rid, ok, message, .. } = &event {
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
struct Endpoint {
    addr: SocketAddr,
    key: String,
}

fn endpoint(args: &Args) -> anyhow::Result<Endpoint> {
    let addr = resolve_server(args)?;
    let key = resolve_key(args).ok_or_else(|| {
        anyhow!("no connection key: pass --key, set GONGGA_KEY, or start the server once to generate one")
    })?;
    Ok(Endpoint { addr, key })
}

/// Why a dial did not produce a link.
enum DialFailure {
    /// Asking again cannot help: the key is wrong, the address is malformed.
    /// Retrying would only hide the reason behind an endless "reconnecting".
    Fatal(anyhow::Error),
    /// Worth retrying: the server is not listening right now.
    Temporary(anyhow::Error),
}

impl DialFailure {
    fn message(&self) -> String {
        match self {
            DialFailure::Fatal(e) | DialFailure::Temporary(e) => e.to_string(),
        }
    }

    fn is_fatal(&self) -> bool {
        matches!(self, DialFailure::Fatal(_))
    }

    fn into_error(self) -> anyhow::Error {
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
async fn dial(endpoint: Endpoint) -> Result<(Link, ServerMessage), DialFailure> {
    let socket = Arc::new(
        UdpSocket::bind("0.0.0.0:0")
            .await
            .map_err(|e| DialFailure::Temporary(e.into()))?,
    );
    let channel = protocol::connect(socket, endpoint.addr, ChannelConfig::default())
        .await
        .map_err(|e| DialFailure::Temporary(anyhow!("{e}")))?;
    let (sink, mut stream) = channel.split();

    let hello = encode_client(&ClientMessage::Hello {
        client: client_info(),
        key: endpoint.key.clone(),
    })
    .map_err(|e| DialFailure::Fatal(e.into()))?;
    sink.send(hello)
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
    Ok((Link { sink, stream }, msg))
}

/// The client's own state, built without touching the network.
///
/// The catalog starts with only the client's half; the server's half arrives
/// with the greeting, which is why a client can exist before its server does.
fn build_client(args: &Args) -> Client {
    let home = home_dir(args.home.as_deref());
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
async fn connect(
    args: &Args,
) -> anyhow::Result<(Client, protocol::ChannelSink, protocol::ChannelStream)> {
    let (link, greeting) = match dial(endpoint(args)?).await {
        Ok(pair) => pair,
        Err(failure) if failure.is_fatal() => return Err(failure.into_error()),
        // One-shot mode's caller is usually a script or the model, so the one
        // thing worth adding to the transport's own words is the usual cause.
        Err(failure) => {
            return Err(anyhow!("{}; is `gongga-server` running?", failure.message()));
        }
    };
    let mut client = build_client(args);
    client.handle(greeting, &mut NullSink::default());
    Ok((client, link.sink, link.stream))
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
fn resolve_server(args: &Args) -> anyhow::Result<SocketAddr> {
    resolve_server_with(args, &process_env)
}

fn resolve_server_with(args: &Args, env: &Env) -> anyhow::Result<SocketAddr> {
    if let Some(s) = &args.server {
        return s.parse().with_context(|| format!("parsing --server {s:?}"));
    }
    if let Some(s) = env("GONGGA_SERVER") {
        return s.parse().with_context(|| format!("parsing GONGGA_SERVER {s:?}"));
    }
    if let Some(bind) = config_value(args, |c| c.server.as_ref().and_then(|s| s.bind.clone())) {
        return bind.parse().with_context(|| format!("parsing bind {bind:?}"));
    }
    Ok(DEFAULT_SERVER.parse().unwrap())
}

/// `--key`, then `GONGGA_KEY`, then `[server] key` in config.toml.
fn resolve_key(args: &Args) -> Option<String> {
    resolve_key_with(args, &process_env)
}

fn resolve_key_with(args: &Args, env: &Env) -> Option<String> {
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

fn config_value(args: &Args, pick: impl Fn(&FileConfig) -> Option<String>) -> Option<String> {
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

    /// `gongga send_message --message "two words" --session main` has to reach
    /// the parser as two arguments, not three.
    #[test]
    fn argv_keeps_an_argument_that_contains_a_space() {
        let argv: Vec<String> = ["send_message", "--message", "two words", "--session", "main"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let line = command_line(&argv);
        let back = shell_words::split(&line).expect("the line parses");
        assert_eq!(back, argv);
    }

    /// One argument is already a line. Quoting it would make the parser see a
    /// single token called `send_message --message hi`, which is not a command.
    #[test]
    fn a_single_argument_is_passed_through_verbatim() {
        let line = command_line(&["send_message --message hi".to_string()]);
        assert_eq!(line, "send_message --message hi");
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
        let args = Args {
            server: Some("10.0.0.5:1234".into()),
            home: Some(d.path().to_path_buf()),
            key: None,
            command: None,
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
        let args = Args {
            server: None,
            home: Some(d.path().to_path_buf()),
            key: None,
            command: None,
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
        let args = Args {
            server: None,
            home: Some(d.path().to_path_buf()),
            key: None,
            command: None,
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
        assert_eq!(
            resolve_key_with(&args, &env).as_deref(),
            Some("from-shell")
        );
    }

    #[test]
    fn key_flag_beats_config() {
        let d = home_with("[server]\nkey = \"abc\"\n");
        let args = Args {
            server: None,
            home: Some(d.path().to_path_buf()),
            key: Some("zzz".into()),
            command: None,
        };
        assert_eq!(resolve_key_with(&args, &no_env).as_deref(), Some("zzz"));
    }

    #[test]
    fn missing_key_yields_nothing() {
        let d = home_with("[server]\nbind = \"127.0.0.1:9999\"\n");
        let args = Args {
            server: None,
            home: Some(d.path().to_path_buf()),
            key: None,
            command: None,
        };
        assert!(resolve_key_with(&args, &no_env).is_none());
    }

    /// Drives `submit` without a terminal or a socket.
    ///
    /// The client is marked connected: these tests are about what a line means,
    /// and an offline client refuses every server command before it gets that
    /// far (see `submit`), which would make every one of them test the same
    /// thing.
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
        s.line(LineKind::Error, "send_message is missing required argument(s): session");
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
    /// Bare text used to be shorthand for `send_message` to the sticky
    /// session, which is what made the UI look like it had a current session.
    /// A sticky `session` still fills in an omitted flag, but it no longer
    /// changes what a line *means*.
    #[test]
    fn bare_text_is_not_a_command() {
        let d = tempfile::tempdir().unwrap();
        let mut sticky = Sticky::load(d.path());
        sticky.set("session", "main").unwrap();
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
                    required: true,
                    value_hint: None,
                    completions: protocol::CompletionSource::None,
                },
            ],
        }]);
        let mut c = client_with(cat, sticky, vec!["main"]);
        let mut s = VecSink(Vec::new());
        assert!(matches!(c.submit("hello there", &mut s), Submission::Rejected));
        assert!(s.0.iter().any(|l| l.contains("unknown command")));
        // The same words, spelled as a command, still go through.
        match c.submit("send_message --message hello --session main", &mut s) {
            Submission::Send(ClientMessage::Command { name, args, .. }) => {
                assert_eq!(name, "send_message");
                assert_eq!(args["message"], "hello");
                assert_eq!(args["session"], "main");
            }
            _ => panic!("expected a command to send"),
        }
    }

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
        match c.submit("send_message --message hi", &mut s) {
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
            c.submit("set_persistant_cmd_argument --name session --value main", &mut s),
            Submission::Local
        ));
        assert_eq!(c.sticky.pairs(), vec![("session".to_string(), "main".to_string())]);
        assert!(matches!(
            c.submit("del_persistant_cmd_argument --name session", &mut s),
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
            c.submit("del_session --name main", &mut s),
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
        assert!(matches!(c.submit("del_session", &mut s), Submission::Rejected));
        assert!(s.0.iter().any(|l| l.contains("missing required")));
    }

}
