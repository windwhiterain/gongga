//! One IM chat per harness session, as an ordinary protocol client.
//! See `docs/notes/bridge-main.md`.

mod channel;
mod chat;
mod config;

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, anyhow};
use clap::Parser;
use protocol::{
    Args as CommandArgs, ChannelConfig, ClientInfo, ClientMessage, Event, ServerMessage,
    SessionStatus, decode_server, encode_client,
};
use tokio::net::UdpSocket;
use tokio::sync::mpsc;

use crate::channel::{ChatId, Incoming, MsgRef, Outbox, Outgoing};
use crate::chat::{ChatState, Dedupe, Pending, Row, clip, final_reply};
use crate::config::{Bridge as BridgeCfg, Config};

const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(5);
const DEFAULT_SERVER: &str = "127.0.0.1:9700";

#[derive(Debug, Parser)]
#[command(
    name = "gongga-bridge",
    about = "Carries IM chats into gongga sessions",
    version
)]
struct Args {
    /// Server address, e.g. `127.0.0.1:9700`. Defaults to `[server] bind`.
    #[arg(long)]
    server: Option<String>,
    /// Harness home holding config.toml and bridge.toml. Defaults to `~/.gongga`.
    #[arg(long)]
    home: Option<PathBuf>,
    /// Connection key. Defaults to `[server] key` in config.toml.
    #[arg(long)]
    key: Option<String>,
}

// ------------------------------- The bridge -------------------------------

/// A message that will never reach the harness, and the apology to send back.
struct Refusal {
    chat: ChatId,
    reply: MsgRef,
    text: String,
}

struct Bridge {
    kind: &'static str,
    cwd: String,
    allowed: Vec<String>,
    ack: bool,
    max_chars: usize,
    /// Per session: where its transcript cursor stands and what is owed.
    chats: HashMap<String, ChatState>,
    seen: Dedupe,
    /// Messages accepted but not yet handed over, oldest first.
    queue: VecDeque<Incoming>,
    /// `get_messages` probes: command id -> the message waiting on the answer.
    probes: HashMap<u64, Incoming>,
    /// Read-backs: command id -> the session being read.
    reads: HashMap<u64, String>,
    /// The rows of the read each session asked for, as they arrive.
    rows: HashMap<String, Vec<Row>>,
    next_id: u64,
}

impl Bridge {
    fn new(kind: &'static str, cfg: BridgeCfg) -> Self {
        Self {
            kind,
            cwd: cfg.cwd,
            allowed: cfg.allowed,
            ack: cfg.ack,
            max_chars: cfg.max_chars,
            chats: HashMap::new(),
            seen: Dedupe::default(),
            queue: VecDeque::new(),
            probes: HashMap::new(),
            reads: HashMap::new(),
            rows: HashMap::new(),
            next_id: Self::seed_command_id(),
        }
    }

    /// A process/time-seeded start avoids collisions with other bridge clients.
    fn seed_command_id() -> u64 {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(0);
        (nanos ^ ((std::process::id() as u64) << 32)) | 1
    }

    /// Screens a delivered message: deduplicated, allow-listed, queued.
    fn accept(&mut self, incoming: Incoming) -> Option<Refusal> {
        if !self.seen.first_time(&incoming.reply.id) {
            tracing::debug!(chat = %incoming.chat.label(), id = %incoming.reply.id, "ignoring a retried delivery");
            return None;
        }
        if !self.allowed.iter().any(|user| user == &incoming.user) {
            tracing::warn!(
                user = %incoming.user,
                chat = %incoming.chat.label(),
                "refused a message from a user that is not in [bridge] allowed"
            );
            // The refusal must carry their id: it is what goes in the allow-list.
            let text = if self.allowed.is_empty() {
                format!(
                    "这个 bot 还没配置白名单，谁都不能用。把你的 id `{}` 填进 bridge.toml 的 [bridge] allowed，再重启。",
                    incoming.user
                )
            } else {
                format!("你没有权限使用这个 bot（你的 id：{}）。", incoming.user)
            };
            return Some(Refusal {
                chat: incoming.chat,
                reply: incoming.reply,
                text,
            });
        }
        self.queue.push_back(incoming);
        None
    }

    /// Hands queued messages to the harness, as far as the state allows.
    ///
    /// # Invariant
    ///
    /// Nothing is written to a transcript whose end has not been read yet: the
    /// read-back would replay the whole history and relay an old answer as new.
    async fn pump(
        &mut self,
        sink: &protocol::ChannelSink,
        out: &Arc<dyn Outbox>,
    ) -> anyhow::Result<()> {
        while let Some(incoming) = self.queue.front().cloned() {
            let session = incoming.chat.session_name(self.kind);
            // A stop is interrupt_session, never cancel: that stops every chat.
            if is_stop(&incoming.text) {
                self.command(
                    sink,
                    "interrupt_session",
                    args_of(&[("session", session.as_str())]),
                )
                .await?;
                self.queue.pop_front();
                if let Some(state) = self.chats.get_mut(&session) {
                    // Forget what was owed, or the next Idle relays it anyway.
                    state.waiting.clear();
                }
                let _ = deliver(
                    out,
                    &incoming.chat,
                    &incoming.reply,
                    1,
                    "已停止这个会话正在跑的任务。",
                )
                .await;
                continue;
            }
            let known = self
                .chats
                .get(&session)
                .and_then(|state| state.last_seq)
                .is_some();
            if !known {
                // One row: the probe only needs `last_seq`, the end of the transcript.
                let id = self
                    .command(
                        sink,
                        "get_messages",
                        args_of(&[("session", &session), ("range", "-1")]),
                    )
                    .await?;
                self.queue.pop_front();
                // A probe reads one row too: start its window clean.
                self.rows.insert(session.clone(), Vec::new());
                self.probes.insert(id, incoming);
                continue;
            }

            // `set_session` creates as well as selects, so send it every time.
            let mut ensure = vec![("session", session.as_str())];
            if !self.cwd.is_empty() {
                ensure.push(("cwd", self.cwd.as_str()));
            }
            self.command(sink, "set_session", args_of(&ensure)).await?;
            self.command(
                sink,
                "send_message",
                args_of(&[
                    ("message", incoming.text.as_str()),
                    ("session", session.as_str()),
                ]),
            )
            .await?;
            self.queue.pop_front();

            let state = self.chats.entry(session.clone()).or_default();
            state.waiting.push_back(Pending {
                chat: incoming.chat.clone(),
                reply: incoming.reply.clone(),
                text: incoming.text.clone(),
            });
            if self.ack {
                let _ = deliver(out, &incoming.chat, &incoming.reply, 1, "收到，正在跑…").await;
            }
            tracing::info!(chat = %incoming.chat.label(), %session, "handed a message to the harness");
        }
        Ok(())
    }

    /// Settles everything tied to the previous connection before using a new one.
    ///
    /// # Invariant
    ///
    /// A probe never ran a user message, so retrying it is safe. A turn already
    /// handed over may have finished while disconnected, and a partial read-back
    /// could not prove it: what was waiting is failed explicitly instead.
    async fn recover(
        &mut self,
        sink: &protocol::ChannelSink,
        out: &Arc<dyn Outbox>,
    ) -> anyhow::Result<()> {
        let mut probes: Vec<(u64, Incoming)> = self.probes.drain().collect();
        probes.sort_by_key(|(id, _)| *id);
        for (_, incoming) in probes.into_iter().rev() {
            self.queue.push_front(incoming);
        }

        self.reads.clear();
        let waiting: Vec<Pending> = self
            .chats
            .values_mut()
            .flat_map(|state| state.waiting.drain(..))
            .collect();
        for pending in waiting {
            let _ = deliver(
                out,
                &pending.chat,
                &pending.reply,
                2,
                "与服务端的连接中断，这一轮结果未知；请重新发送。",
            )
            .await;
        }
        self.pump(sink, out).await
    }

    async fn on_event(
        &mut self,
        message: ServerMessage,
        sink: &protocol::ChannelSink,
        out: &Arc<dyn Outbox>,
    ) -> anyhow::Result<()> {
        let ServerMessage::Event(event) = message else {
            return Ok(());
        };
        match event {
            Event::Response(event) if event.turn == protocol::REPLAY_TURN => {
                // A read sends its rows to the connection that asked and no
                // further: every part here answers one of our reads.
                let session = event.session.clone();
                let row = Row::of(event.part, &event.text);
                self.rows.entry(session).or_default().push(row);
                Ok(())
            }
            Event::CmdResult {
                id,
                ok,
                message,
                data,
            } => self.on_result(id, ok, message, data, sink, out).await,
            Event::Status {
                session,
                status,
                detail,
            } => match status {
                SessionStatus::Idle => self.on_idle(&session, sink).await,
                SessionStatus::Error => self.on_error(&session, detail, out).await,
                SessionStatus::Running => Ok(()),
            },
            // A live stream, tool output and the command echo are for a human
            // watching a terminal; a chat window is not one.
            _ => Ok(()),
        }
    }

    async fn on_result(
        &mut self,
        id: u64,
        ok: bool,
        message: String,
        data: Option<serde_json::Value>,
        sink: &protocol::ChannelSink,
        out: &Arc<dyn Outbox>,
    ) -> anyhow::Result<()> {
        // A probe: now we know where the transcript ends, so the parked message
        // can go in.
        if let Some(incoming) = self.probes.remove(&id) {
            let session = incoming.chat.session_name(self.kind);
            match data.as_ref().and_then(|value| value["last_seq"].as_i64()) {
                Some(last) => {
                    self.chats.entry(session).or_default().last_seq = Some(last);
                }
                None => {
                    let text = format!("读不到会话 transcript，没有执行：{message}");
                    let _ = deliver(
                        out,
                        &incoming.chat,
                        &incoming.reply,
                        2,
                        &clip(&text, self.max_chars),
                    )
                    .await;
                    return Ok(());
                }
            }
            self.queue.push_front(incoming);
            return self.pump(sink, out).await;
        }

        // A read-back: the answer is in the transcript, relay it.
        if let Some(session) = self.reads.remove(&id) {
            let Some(pending) = self
                .chats
                .get(&session)
                .and_then(|state| state.waiting.front().cloned())
            else {
                return Ok(());
            };
            if !ok {
                let text = clip(&format!("读回回答失败：{message}"), self.max_chars);
                deliver(out, &pending.chat, &pending.reply, 2, &text).await?;
                if let Some(state) = self.chats.get_mut(&session) {
                    state.waiting.pop_front();
                }
                return Ok(());
            }
            // The rows came in as events before this result; take what was read.
            let rows = self.rows.remove(&session).unwrap_or_default();
            // The read started at the cursor, so everything in these rows is owed.
            let text = clip(&final_reply(&rows, &pending.text), self.max_chars);
            let last = data.as_ref().and_then(|value| value["last_seq"].as_i64());
            // Delivery commits the turn: on failure the pending reply and cursor
            // stay for recovery to settle.
            deliver(out, &pending.chat, &pending.reply, 2, &text).await?;
            if let Some(state) = self.chats.get_mut(&session) {
                if let Some(seq) = last {
                    state.last_seq = Some(seq);
                }
                state.waiting.pop_front();
            }
            return Ok(());
        }

        if !ok {
            tracing::warn!(id, %message, "a command was refused by the server");
        }
        Ok(())
    }

    async fn on_idle(&mut self, session: &str, sink: &protocol::ChannelSink) -> anyhow::Result<()> {
        let Some(state) = self.chats.get(session) else {
            return Ok(());
        };
        // Idle also fires for turns we did not start; read back only what is owed.
        if state.waiting.is_empty() {
            return Ok(());
        }
        // The server answers a range: from the row after the cursor on is exactly
        // what this chat is still owed an answer for.
        let from = format!("{}:", state.last_seq.unwrap_or(0) + 1);
        let id = self
            .command(
                sink,
                "get_messages",
                args_of(&[("session", session), ("range", from.as_str())]),
            )
            .await?;
        self.reads.insert(id, session.to_string());
        // Whatever arrives from now on belongs to this read.
        self.rows.insert(session.to_string(), Vec::new());
        Ok(())
    }

    async fn on_error(
        &mut self,
        session: &str,
        detail: Option<String>,
        out: &Arc<dyn Outbox>,
    ) -> anyhow::Result<()> {
        let Some(state) = self.chats.get(session) else {
            return Ok(());
        };
        let Some(pending) = state.waiting.front().cloned() else {
            return Ok(());
        };
        let detail = detail.unwrap_or_else(|| "看服务端日志".to_string());
        let text = clip(&format!("这一轮出错了：{detail}"), self.max_chars);
        deliver(out, &pending.chat, &pending.reply, 2, &text).await?;
        if let Some(state) = self.chats.get_mut(session) {
            state.waiting.pop_front();
        }
        Ok(())
    }

    async fn command(
        &mut self,
        sink: &protocol::ChannelSink,
        name: &str,
        args: CommandArgs,
    ) -> anyhow::Result<u64> {
        let id = self.next_id;
        self.next_id += 1;
        let message = ClientMessage::Command {
            id,
            name: name.to_string(),
            args,
        };
        sink.send(encode_client(&message)?)
            .await
            .map_err(|e| anyhow!("sending {name} to the server: {e}"))?;
        Ok(id)
    }
}

// -------------------------------- Plumbing --------------------------------

/// Delivers one chat message, logging and returning the failure so callers can
/// decide whether to commit.
async fn deliver(
    out: &Arc<dyn Outbox>,
    chat: &ChatId,
    reference: &MsgRef,
    seq: u32,
    text: &str,
) -> anyhow::Result<()> {
    let outgoing = Outgoing {
        chat: chat.clone(),
        text: text.to_string(),
        reply_to: Some((reference.clone(), seq)),
    };
    let result = out.send(outgoing).await;
    if let Err(e) = &result {
        tracing::warn!(chat = %chat.label(), error = %e, "could not deliver a reply");
    }
    result
}

/// `/stop` and `/cancel` end the turn of the conversation that sent them.
/// See `docs/notes/bridge-main.md`.
fn is_stop(text: &str) -> bool {
    matches!(text.trim(), "/stop" | "/cancel")
}

fn args_of(pairs: &[(&str, &str)]) -> CommandArgs {
    pairs
        .iter()
        .map(|(key, value)| (key.to_string(), value.to_string()))
        .collect::<BTreeMap<_, _>>()
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "gongga_bridge=info,info".into()),
        )
        .init();

    let args = Args::parse();
    let home = home_dir(args.home.as_deref());
    let cfg = Config::load(&home)?;

    if cfg.bridge.allowed.is_empty() {
        tracing::warn!(
            path = %Config::path(&home).display(),
            "nobody is allowed to talk to this bot yet: the first refusal names the id to add"
        );
    }

    let channels = channel::qq::open(&cfg.qq).await?;
    let kind = channels.kind();
    let outbox = channels.outbox.clone();
    let mut inbox = channels.inbox;

    // The socket has its own task, so a platform reconnect cannot stall us.
    let (sender, mut incoming) = mpsc::channel::<Incoming>(64);
    tokio::spawn(async move {
        loop {
            match inbox.recv().await {
                Ok(message) => {
                    if sender.send(message).await.is_err() {
                        break;
                    }
                }
                Err(e) => {
                    tracing::warn!(error = %e, "channel read failed; retrying");
                    tokio::time::sleep(Duration::from_secs(3)).await;
                }
            }
        }
    });

    let mut bridge = Bridge::new(kind, cfg.bridge);
    // Redial here, not in the platform loop: the harness is replaced, we are not.
    let mut backoff = REDIAL_MIN;
    loop {
        let (sink, mut stream) = match dial(&args, &home).await {
            Ok(pair) => pair,
            // A refusal is an answer: the server is up and does not want this
            // bridge, so retrying would be refused again.
            Err(e) if e.downcast_ref::<Refused>().is_some() => return Err(e),
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    retry_in = ?backoff,
                    "could not reach the harness; the platform connection stays up"
                );
                tokio::time::sleep(backoff).await;
                backoff = (backoff * 2).min(REDIAL_MAX);
                continue;
            }
        };
        backoff = REDIAL_MIN;
        let link = match bridge.recover(&sink, &outbox).await {
            Err(e) => e,
            Ok(()) => loop {
                tokio::select! {
                    Some(message) = incoming.recv() => {
                        match bridge.accept(message) {
                            None => {
                                if let Err(e) = bridge.pump(&sink, &outbox).await {
                                    break e;
                                }
                            }
                            Some(refusal) => {
                                let _ = deliver(&outbox, &refusal.chat, &refusal.reply, 1, &refusal.text).await;
                            }
                        }
                    }
                    frame = stream.recv() => {
                        let Some(bytes) = frame else {
                            break anyhow!("the server closed the connection");
                        };
                        if let Err(e) = bridge.on_event(decode_server(&bytes)?, &sink, &outbox).await {
                            break e;
                        }
                    }
                }
            },
        };
        // Both halves go with the link; the next dial makes new ones.
        drop((sink, stream));
        tracing::warn!(error = %link, retry_in = ?backoff, "lost the harness; redialing");
        tokio::time::sleep(backoff).await;
        backoff = (backoff * 2).min(REDIAL_MAX);
    }
}

/// First redial delay and the ceiling it backs off to; retrying never stops.
/// See `docs/notes/bridge-main.md`.
const REDIAL_MIN: Duration = Duration::from_secs(1);
const REDIAL_MAX: Duration = Duration::from_secs(30);

/// The harness answered, and the answer was no.
#[derive(Debug)]
struct Refused(String);

impl std::fmt::Display for Refused {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "the server refused the bridge: {}", self.0)
    }
}

impl std::error::Error for Refused {}

// ----------------------- Connection to the harness -----------------------

/// Connects and authenticates, exactly as the CLI does.
async fn dial(
    args: &Args,
    home: &Path,
) -> anyhow::Result<(protocol::ChannelSink, protocol::ChannelStream)> {
    let addr = resolve_server(args, home)?;
    let key = resolve_key(args, home).ok_or_else(|| {
        anyhow!(
            "no connection key: pass --key, set GONGGA_KEY, or start the server once so it \
             writes one into config.toml"
        )
    })?;
    let socket = Arc::new(UdpSocket::bind("0.0.0.0:0").await?);
    let channel = protocol::connect(socket, addr, ChannelConfig::default()).await?;
    let (sink, mut stream) = channel.split();
    let hello = ClientMessage::Hello {
        client: ClientInfo {
            version: env!("CARGO_PKG_VERSION").to_string(),
            label: format!("bridge/pid-{}", std::process::id()),
        },
        key,
    };
    sink.send(encode_client(&hello)?)
        .await
        .map_err(|e| anyhow!("handshake failed: {e}"))?;

    let greeting = tokio::time::timeout(HANDSHAKE_TIMEOUT, stream.recv())
        .await
        .map_err(|_| {
            anyhow!("no answer from {addr} within {HANDSHAKE_TIMEOUT:?}; is a server running?")
        })?
        .ok_or_else(|| anyhow!("the server closed the connection during the handshake"))?;
    match decode_server(&greeting).context("decoding the greeting")? {
        ServerMessage::Denied { reason } => return Err(anyhow::Error::new(Refused(reason))),
        ServerMessage::Hello {
            server, sessions, ..
        } => {
            tracing::info!(%addr, model = %server.model, sessions = sessions.len(), "connected to the harness");
        }
        other => tracing::warn!(?other, "unexpected greeting"),
    }
    Ok((sink, stream))
}

fn home_dir(explicit: Option<&Path>) -> PathBuf {
    explicit
        .map(Path::to_path_buf)
        .or_else(|| std::env::var_os("GONGGA_HOME").map(PathBuf::from))
        .or_else(|| dirs::home_dir().map(|home| home.join(".gongga")))
        .unwrap_or_else(|| PathBuf::from(".gongga"))
}

#[derive(Default, serde::Deserialize)]
struct FileConfig {
    #[serde(default)]
    server: FileServer,
}

#[derive(Default, serde::Deserialize)]
struct FileServer {
    bind: Option<String>,
    key: Option<String>,
}

fn file_config(home: &Path) -> FileConfig {
    std::fs::read_to_string(home.join("config.toml"))
        .ok()
        .and_then(|text| toml::from_str(&text).ok())
        .unwrap_or_default()
}

fn resolve_server(args: &Args, home: &Path) -> anyhow::Result<SocketAddr> {
    if let Some(server) = &args.server {
        return server
            .parse()
            .with_context(|| format!("parsing --server {server:?}"));
    }
    if let Ok(server) = std::env::var("GONGGA_SERVER") {
        if !server.is_empty() {
            return server
                .parse()
                .with_context(|| format!("parsing GONGGA_SERVER {server:?}"));
        }
    }
    if let Some(bind) = file_config(home).server.bind {
        return bind
            .parse()
            .with_context(|| format!("parsing bind {bind:?}"));
    }
    Ok(DEFAULT_SERVER.parse().unwrap())
}

fn resolve_key(args: &Args, home: &Path) -> Option<String> {
    if let Some(key) = &args.key {
        return Some(key.clone());
    }
    if let Ok(key) = std::env::var("GONGGA_KEY") {
        if !key.is_empty() {
            return Some(key);
        }
    }
    file_config(home).server.key
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    #[derive(Default)]
    struct TestOutbox {
        fail: bool,
        sent: Mutex<Vec<Outgoing>>,
    }

    #[async_trait::async_trait]
    impl Outbox for TestOutbox {
        async fn send(&self, out: Outgoing) -> anyhow::Result<()> {
            if self.fail {
                anyhow::bail!("test delivery failure");
            }
            self.sent.lock().unwrap().push(out);
            Ok(())
        }
    }

    /// A sink with no live peer; keeping its sender alive lets recovery queue probes
    /// with no network fixture.
    async fn test_sink() -> (protocol::ChannelSink, mpsc::Sender<protocol::frame::Frame>) {
        let socket = Arc::new(UdpSocket::bind("127.0.0.1:0").await.unwrap());
        let peer = socket.local_addr().unwrap();
        let (tx, rx) = mpsc::channel(8);
        let channel = protocol::accept(socket, peer, rx, ChannelConfig::default(), 1);
        let (sink, _stream) = channel.split();
        (sink, tx)
    }

    fn incoming(id: &str) -> Incoming {
        Incoming {
            chat: ChatId::Direct("user".into()),
            user: "user".into(),
            text: "hello".into(),
            reply: MsgRef { id: id.into() },
        }
    }

    fn pending(id: &str) -> Pending {
        let incoming = incoming(id);
        Pending {
            chat: incoming.chat,
            reply: incoming.reply,
            text: incoming.text,
        }
    }

    /// Only the slash form is a control message; "cancel the commit" is a task.
    #[test]
    fn only_the_slash_form_stops_a_turn() {
        assert!(is_stop("/stop"));
        assert!(is_stop("  /cancel "));
        assert!(!is_stop("stop"));
        assert!(!is_stop("cancel the last commit"));
        assert!(!is_stop("请取消上一条"));
    }

    #[test]
    fn command_ids_do_not_start_at_one() {
        let id = Bridge::seed_command_id();
        assert_ne!(id, 1);
        assert_eq!(id & 1, 1);
    }

    #[tokio::test]
    async fn failed_final_delivery_does_not_commit_cursor_or_waiting() {
        let mut bridge = Bridge::new("qq", BridgeCfg::default());
        let session = "qq-d-user".to_string();
        bridge.chats.insert(
            session.clone(),
            ChatState {
                last_seq: Some(4),
                waiting: VecDeque::from([pending("m1")]),
            },
        );
        bridge.reads.insert(7, session);
        let out: Arc<dyn Outbox> = Arc::new(TestOutbox {
            fail: true,
            ..TestOutbox::default()
        });
        let (sink, _frames) = test_sink().await;
        let data = serde_json::json!({
            "last_seq": 6,
            "messages": [{"seq": 6, "kind": "text", "role": "assistant", "text": "done"}]
        });

        assert!(
            bridge
                .on_result(7, true, "ok".into(), Some(data), &sink, &out)
                .await
                .is_err()
        );
        let state = &bridge.chats["qq-d-user"];
        assert_eq!(state.last_seq, Some(4));
        assert_eq!(state.waiting.len(), 1);
    }

    #[tokio::test]
    async fn reconnect_retries_probes_and_fails_old_waiting() {
        let mut bridge = Bridge::new("qq", BridgeCfg::default());
        bridge.next_id = 100;
        let mut probe = incoming("probe");
        probe.chat = ChatId::Direct("new-user".into());
        bridge.probes.insert(1, probe);
        bridge.reads.insert(2, "qq-d-user".into());
        bridge.chats.insert(
            "qq-d-user".into(),
            ChatState {
                last_seq: Some(4),
                waiting: VecDeque::from([pending("waiting")]),
            },
        );
        let delivered = Arc::new(TestOutbox::default());
        let out: Arc<dyn Outbox> = delivered.clone();
        let (sink, _frames) = test_sink().await;

        bridge.recover(&sink, &out).await.unwrap();

        assert!(bridge.reads.is_empty());
        assert_eq!(bridge.probes.len(), 1);
        assert!(bridge.probes.contains_key(&100));
        assert!(bridge.chats["qq-d-user"].waiting.is_empty());
        let sent = delivered.sent.lock().unwrap();
        assert_eq!(sent.len(), 1);
        assert!(sent[0].text.contains("结果未知"), "{}", sent[0].text);
    }
}
