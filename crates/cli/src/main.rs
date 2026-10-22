//! `gongga`: the terminal client.
//!
//! The client owns the command surface — it knows the catalog the server
//! publishes, keeps the sticky arguments, and turns an input line into either a
//! local action or a message on the wire. The TUI is driven through [`Sink`] and
//! never learns what a command is.

mod catalog;
mod commands;

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
use tui::LineKind;

use crate::catalog::Catalog;
use crate::commands::{Sticky, parse_args, render_args};

const DEFAULT_SERVER: &str = "127.0.0.1:9700";
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(5);

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
}

impl Client {
    fn new(catalog: Catalog, sticky: Sticky) -> Self {
        Self {
            catalog,
            sticky,
            sessions: Vec::new(),
            model: "-".to_string(),
            next_id: 1,
        }
    }

    fn next_id(&mut self) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        id
    }

    /// The one place a session is inferred: `cancel` (Ctrl+X), which has to
    /// name one and takes it from the sticky `session` argument.
    ///
    /// Deliberately *not* "the first session the server happens to have". A
    /// default the user did not ask for is exactly the hidden state the SPEC
    /// avoids by keeping `persistant_cmd_arguments` explicit.
    fn sticky_session(&self) -> Option<String> {
        self.sticky.get("session").cloned()
    }

    fn handle(&mut self, msg: ServerMessage, sink: &mut dyn Sink) {
        match msg {
            ServerMessage::Hello { server, commands, sessions, .. } => {
                self.model = server.model.clone();
                self.sessions = sessions;
                self.catalog.set_server_commands(commands);
                self.catalog.set_sessions(self.session_names());
                self.sync_sticky();
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
    /// sessions exist.
    fn refresh(&self, sink: &mut dyn Sink) {
        sink.status(&format!(
            "model {} | {} session(s)",
            self.model,
            self.sessions.len()
        ));
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

    /// Ctrl+X: stop the turn of the sticky session, if there is one.
    fn cancel(&mut self, sink: &mut dyn Sink) -> Option<ClientMessage> {
        match self.sticky_session() {
            Some(session) => {
                sink.line(LineKind::System, &format!("cancelling {session}"));
                Some(ClientMessage::Cancel { session })
            }
            None => {
                sink.line(
                    LineKind::Error,
                    "no session to cancel: set a sticky `session` first",
                );
                None
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Front-ends
// ---------------------------------------------------------------------------

async fn interactive(args: &Args) -> anyhow::Result<()> {
    let (mut client, sink, mut stream) = connect(args).await?;
    let mut ui = tui::Tui::new(Arc::new(client.catalog.clone()))?;
    client.refresh(&mut TuiSink(&mut ui));

    let result = loop {
        tokio::select! {
            ui_event = ui.next_event() => {
                match ui_event {
                    None => break Ok(()),
                    Some(tui::UiEvent::Quit) => break Ok(()),
                    Some(tui::UiEvent::Cancel) => {
                        let msg = client.cancel(&mut TuiSink(&mut ui));
                        if let Some(msg) = msg {
                            send(&mut ui, &sink, &msg).await;
                        }
                    }
                    Some(tui::UiEvent::Submit(line)) => {
                        match client.submit(&line, &mut TuiSink(&mut ui)) {
                            Submission::Local | Submission::Rejected => {}
                            Submission::Send(msg) => send(&mut ui, &sink, &msg).await,
                        }
                    }
                }
            }
            maybe = stream.recv() => {
                match maybe {
                    Some(bytes) => match decode_server(&bytes) {
                        Ok(msg) => client.handle(msg, &mut TuiSink(&mut ui)),
                        Err(e) => TuiSink(&mut ui).line(LineKind::Error, &format!("bad message: {e}")),
                    },
                    None => {
                        TuiSink(&mut ui).line(LineKind::System, "server disconnected");
                        break Ok(());
                    }
                }
            }
        }
    };

    ui.restore()?;
    result
}

async fn send(ui: &mut tui::Tui, sink: &protocol::ChannelSink, msg: &ClientMessage) {
    match encode_client(msg) {
        Ok(bytes) => {
            if let Err(e) = sink.send(bytes).await {
                ui.push(LineKind::Error, format!("send failed: {e}"));
            }
        }
        Err(e) => ui.push(LineKind::Error, format!("encoding failed: {e}")),
    }
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

/// Connects, authenticates, and returns a client that already knows the
/// server's command catalog. Shared by both front-ends.
async fn connect(
    args: &Args,
) -> anyhow::Result<(Client, protocol::ChannelSink, protocol::ChannelStream)> {
    let addr = resolve_server(args)?;
    let key = resolve_key(args).ok_or_else(|| {
        anyhow!("no connection key: pass --key, set GONGGA_KEY, or start the server once to generate one")
    })?;
    let socket = Arc::new(UdpSocket::bind("0.0.0.0:0").await?);
    let channel = protocol::connect(socket, addr, ChannelConfig::default()).await?;
    let (sink, mut stream) = channel.split();

    sink.send(encode_client(&ClientMessage::Hello {
        client: client_info(),
        key,
    })?)
    .await
    .map_err(|e| anyhow!("handshake failed: {e}"))?;

    // The greeting is the first thing the server sends, so waiting for it is
    // both the handshake confirmation and the command catalog download.
    let greeting = tokio::time::timeout(HANDSHAKE_TIMEOUT, stream.recv())
        .await
        .map_err(|_| anyhow!("no response from {addr} within {HANDSHAKE_TIMEOUT:?}; is `gongga-server` running?"))?
        .ok_or_else(|| anyhow!("server closed the connection during handshake"))?;

    let home = home_dir(args.home.as_deref());
    let catalog = Catalog::new();
    let sticky = Sticky::load(&home);
    catalog.set_sticky_names(&sticky.names());
    let mut client = Client::new(catalog, sticky);
    match decode_server(&greeting).context("decoding server greeting")? {
        ServerMessage::Denied { reason } => anyhow::bail!("server refused the connection: {reason}"),
        msg => client.handle(msg, &mut NullSink::default()),
    }
    Ok((client, sink, stream))
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

/// `--server`, then `GONGGA_SERVER`, then `[server] bind` in config.toml.
fn resolve_server(args: &Args) -> anyhow::Result<SocketAddr> {
    if let Some(s) = &args.server {
        return s.parse().with_context(|| format!("parsing --server {s:?}"));
    }
    if let Some(s) = std::env::var("GONGGA_SERVER").ok().filter(|v| !v.is_empty()) {
        return s.parse().with_context(|| format!("parsing GONGGA_SERVER {s:?}"));
    }
    if let Some(bind) = config_value(args, |c| c.server.as_ref().and_then(|s| s.bind.clone())) {
        return bind.parse().with_context(|| format!("parsing bind {bind:?}"));
    }
    Ok(DEFAULT_SERVER.parse().unwrap())
}

/// `--key`, then `GONGGA_KEY`, then `[server] key` in config.toml.
fn resolve_key(args: &Args) -> Option<String> {
    if let Some(k) = &args.key {
        return Some(k.clone());
    }
    if let Some(k) = std::env::var("GONGGA_KEY").ok().filter(|v| !v.is_empty()) {
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

    #[test]
    fn explicit_flag_wins() {
        let d = home_with("[server]\nbind = \"127.0.0.1:9999\"\nkey = \"abc\"\n");
        let args = Args {
            server: Some("10.0.0.5:1234".into()),
            home: Some(d.path().to_path_buf()),
            key: None,
            command: None,
        };
        assert_eq!(resolve_server(&args).unwrap().to_string(), "10.0.0.5:1234");
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
        assert_eq!(resolve_server(&args).unwrap().to_string(), "127.0.0.1:9999");
        assert_eq!(resolve_key(&args).as_deref(), Some("abc"));
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
        assert_eq!(resolve_key(&args).as_deref(), Some("zzz"));
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
        assert!(resolve_key(&args).is_none());
    }

    /// Drives `submit` without a terminal or a socket.
    fn client_with(catalog: Catalog, sticky: Sticky, sessions: Vec<&str>) -> Client {
        let mut c = Client::new(catalog, sticky);
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
            args: vec![],
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
            args: vec![protocol::ArgSpec {
                name: "session".into(),
                summary: "s".into(),
                required: false,
                value_hint: None,
                completions: protocol::CompletionSource::None,
            }],
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
        assert_eq!(c.sticky.get("session").map(String::as_str), Some("main"));
        assert!(matches!(
            c.submit("del_persistant_cmd_argument --name session", &mut s),
            Submission::Local
        ));
        assert!(c.sticky.get("session").is_none());
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
