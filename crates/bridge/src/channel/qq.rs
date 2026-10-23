//! QQ 官方机器人通道（QQ 开放平台 v2 API）。
//!
//! Why this platform first: it is the only domestic one that hands events to a
//! plain Rust program without an SDK. Both other serious options (飞书 long
//! connection, 钉钉 Stream) frame their events in protobuf and ship SDKs only
//! for Go/Python/Node/Java; QQ's gateway is JSON over a WebSocket that
//! `tokio-tungstenite` already speaks.
//!
//! Two halves, because they need different things:
//! * [`Inbox`] owns the gateway socket: identify, heartbeat, reconnect.
//! * [`Outbox`] is just the HTTP API, so a reply can be sent from the bridge
//!   loop without touching the socket.
//!
//! Platform limits that shape the code (官方文档):
//! * 被动回复只对 5 分钟内的消息有效，一条消息最多回 5 次（`msg_seq` 1~5）。
//!   所以「先回一句收到、跑完再回结果」要按 seq 计数，超时就必须改成主动消息。
//! * 主动消息有频控（认证主体 60/min，未认证 30/min，单个群 20/min，
//!   每群每天 1000 条）。一个人用远够。
//! * 群消息不支持流式，所以回复必须是拼好的整段。

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context, anyhow};
use async_trait::async_trait;
use futures_util::stream::{SplitSink, SplitStream};
use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use tokio::net::TcpStream;
use tokio_tungstenite::tungstenite::Message as Ws;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};

use super::{Channels, ChatId, Inbox as InboxTrait, Incoming, MsgRef, Outbox as OutboxTrait, Outgoing};
use crate::config::Qq as Config;

/// 群与单聊的 @ 消息。`1 << 25` covers `GROUP_AT_MESSAGE_CREATE` and
/// `C2C_MESSAGE_CREATE`; the guild intents are deliberately not requested --
/// this bridge has no business in a channel.
const INTENTS: u32 = 1 << 25;

const TOKEN_URL: &str = "https://bots.qq.com/app/getAppAccessToken";
const LIVE_API: &str = "https://api.sgroup.qq.com";
const SANDBOX_API: &str = "https://sandbox.api.sgroup.qq.com";

type Socket = WebSocketStream<MaybeTlsStream<TcpStream>>;
type Sink = SplitSink<Socket, Ws>;
type Source = SplitStream<Socket>;

/// Opens the channel, or says exactly what is missing.
pub async fn open(cfg: &Config) -> anyhow::Result<Channels> {
    if cfg.app_id.trim().is_empty() || cfg.app_secret.trim().is_empty() {
        anyhow::bail!(
            "qq is not configured: set [qq] app_id / app_secret in bridge.toml, \
             or export GONGGA_QQ_APP_ID / GONGGA_QQ_APP_SECRET"
        );
    }
    let http = reqwest::Client::builder()
        .timeout(Duration::from_secs(20))
        .build()
        .context("building the http client")?;
    let token = Arc::new(Token::new(cfg.clone(), http.clone()));
    let inbox = Box::new(Inbox {
        cfg: cfg.clone(),
        http: http.clone(),
        token: token.clone(),
        socket: None,
        heartbeat: Duration::from_secs(30),
        next_ping: Instant::now(),
        last_s: None,
        backoff: Duration::from_secs(1),
    });
    let outbox = Arc::new(Outbox { cfg: cfg.clone(), http, token });
    Ok(Channels { inbox, outbox })
}

// ---------------------------------------------------------------------------
// Access token
// ---------------------------------------------------------------------------

/// `access_token` lives two hours. Fetched lazily and shared by both halves, so
/// connecting the socket does not burn a second round trip on the same value.
struct Token {
    cfg: Config,
    http: reqwest::Client,
    cache: Mutex<Option<(String, Instant)>>,
}

impl Token {
    fn new(cfg: Config, http: reqwest::Client) -> Self {
        Self { cfg, http, cache: Mutex::new(None) }
    }

    async fn get(&self) -> anyhow::Result<String> {
        // Scoped so the guard is dropped before the await below: a std
        // mutex held across an await is how a bridge deadlocks itself.
        let cached = { self.cache.lock().unwrap().clone() };
        if let Some((token, until)) = cached {
            if Instant::now() < until {
                return Ok(token);
            }
        }
        let body: Value = self
            .http
            .post(TOKEN_URL)
            .json(&json!({
                "appId": self.cfg.app_id,
                "clientSecret": self.cfg.app_secret,
            }))
            .send()
            .await
            .context("requesting an app access token")?
            .error_for_status()
            .context("the token endpoint refused the credentials")?
            .json()
            .await
            .context("decoding the token response")?;
        let token = body["access_token"]
            .as_str()
            .ok_or_else(|| anyhow!("no access_token in the response: {body}"))?
            .to_string();
        // `expires_in` comes back as a string on some deployments and a number
        // on others; both have been seen in the wild.
        let ttl = loose_u64(&body["expires_in"]).unwrap_or(7200);
        // A minute early: a token that expires mid-request is a 401 for no
        // reason anyone can debug later.
        *self.cache.lock().unwrap() =
            Some((token.clone(), Instant::now() + Duration::from_secs(ttl.saturating_sub(60))));
        Ok(token)
    }
}

// ---------------------------------------------------------------------------
// Inbound: the gateway socket
// ---------------------------------------------------------------------------

pub struct Inbox {
    cfg: Config,
    http: reqwest::Client,
    token: Arc<Token>,
    socket: Option<(Sink, Source)>,
    heartbeat: Duration,
    next_ping: Instant,
    /// Last sequence number seen. Sent with the heartbeat so the platform can
    /// tell a live-but-quiet connection from a dead one.
    last_s: Option<i64>,
    backoff: Duration,
}

enum Step {
    Continue,
    Reconnect(String),
    Deliver(Incoming),
}

impl Inbox {
    fn api(&self) -> &'static str {
        if self.cfg.sandbox { SANDBOX_API } else { LIVE_API }
    }

    /// Connects, says hello, and identifies. Errors here are retried by
    /// [`recv`](Self::recv), so a flapping network does not kill the bridge.
    async fn connect(&mut self) -> anyhow::Result<()> {
        let token = self.token.get().await?;
        let url = self.gateway(&token).await?;
        let (socket, _response) = tokio_tungstenite::connect_async(url.as_str())
            .await
            .with_context(|| format!("connecting to {url}"))?;
        let (tx, rx) = socket.split();
        self.socket = Some((tx, rx));

        // op 10 hello carries the interval the platform wants to hear from us.
        let hello = self.read_frame().await.context("waiting for op 10 hello")?;
        self.heartbeat = loose_u64(&hello["d"]["heartbeat_interval"])
            .map(Duration::from_millis)
            .unwrap_or(Duration::from_secs(30));
        self.next_ping = Instant::now() + self.heartbeat;

        let identify = json!({
            "op": 2,
            "d": {
                "token": format!("QQBot {token}"),
                "intents": INTENTS,
                "properties": { "$os": std::env::consts::OS, "$device": "gongga-bridge" },
            }
        });
        self.write(Ws::Text(identify.to_string())).await?;
        tracing::info!(gateway = %url, sandbox = self.cfg.sandbox, "qq gateway connected");
        Ok(())
    }

    async fn gateway(&self, token: &str) -> anyhow::Result<String> {
        let url = format!("{}/gateway", self.api());
        let body: Value = self
            .http
            .get(&url)
            .header("Authorization", format!("QQBot {token}"))
            .header("X-Union-Appid", &self.cfg.app_id)
            .send()
            .await
            .context("asking for the gateway url")?
            .error_for_status()
            .context("the gateway url endpoint refused the token")?
            .json()
            .await
            .context("decoding the gateway response")?;
        body["url"]
            .as_str()
            .map(str::to_string)
            .ok_or_else(|| anyhow!("no gateway url in the response: {body}"))
    }

    async fn read_frame(&mut self) -> anyhow::Result<Value> {
        loop {
            let Some((_, source)) = self.socket.as_mut() else {
                return Err(anyhow!("not connected"));
            };
            match source.next().await {
                Some(Ok(Ws::Text(text))) => {
                    return serde_json::from_str(&text).context("decoding a gateway frame");
                }
                Some(Ok(Ws::Close(_))) | None => {
                    return Err(anyhow!("the gateway closed the connection"));
                }
                // Ping/pong/binary carry nothing we act on.
                Some(Ok(_)) => continue,
                Some(Err(e)) => return Err(anyhow!("gateway read failed: {e}")),
            }
        }
    }

    async fn write(&mut self, frame: Ws) -> anyhow::Result<()> {
        let Some((sink, _)) = self.socket.as_mut() else {
            return Err(anyhow!("not connected"));
        };
        sink.send(frame).await.map_err(|e| anyhow!("gateway write failed: {e}"))
    }

    /// One cycle: either the heartbeat is due, or a frame arrives.
    async fn poll(&mut self) -> Step {
        let deadline = tokio::time::Instant::from_std(self.next_ping);
        let Some((sink, source)) = self.socket.as_mut() else {
            return Step::Reconnect("not connected".into());
        };
        tokio::select! {
            _ = tokio::time::sleep_until(deadline) => {
                let d = match self.last_s {
                    Some(s) => json!(s),
                    None => Value::Null,
                };
                if let Err(e) = sink.send(Ws::Text(json!({"op": 1, "d": d}).to_string())).await {
                    return Step::Reconnect(format!("heartbeat failed: {e}"));
                }
                self.next_ping = Instant::now() + self.heartbeat;
                Step::Continue
            }
            frame = source.next() => match frame {
                Some(Ok(Ws::Text(text))) => {
                    let value: Value = match serde_json::from_str(&text) {
                        Ok(v) => v,
                        Err(e) => {
                            tracing::warn!(error = %e, "dropping an unparsable gateway frame");
                            return Step::Continue;
                        }
                    };
                    if let Some(s) = value.get("s").and_then(Value::as_i64) {
                        self.last_s = Some(s);
                    }
                    match value["op"].as_u64() {
                        // op 0 is a dispatch; only message events matter here.
                        Some(0) => {
                            let kind = value["t"].as_str().unwrap_or_default();
                            match parse_dispatch(kind, &value["d"]) {
                                Some(incoming) => Step::Deliver(incoming),
                                None => Step::Continue,
                            }
                        }
                        // op 7 asks us to reconnect, op 9 says the session is
                        // gone. Neither is recoverable in place.
                        Some(op @ (7 | 9)) => Step::Reconnect(format!("op {op}")),
                        // op 11 is the heartbeat acknowledgement.
                        _ => Step::Continue,
                    }
                }
                Some(Ok(Ws::Close(_))) => Step::Reconnect("closed".into()),
                Some(Ok(_)) => Step::Continue,
                Some(Err(e)) => Step::Reconnect(format!("{e}")),
                None => Step::Reconnect("stream ended".into()),
            },
        }
    }
}

#[async_trait]
impl InboxTrait for Inbox {
    fn kind(&self) -> &'static str {
        "qq"
    }

    async fn recv(&mut self) -> anyhow::Result<Incoming> {
        loop {
            if self.socket.is_none() {
                match self.connect().await {
                    Ok(()) => self.backoff = Duration::from_secs(1),
                    Err(e) => {
                        tracing::warn!(error = %e, retry_in = ?self.backoff, "qq gateway connect failed");
                        tokio::time::sleep(self.backoff).await;
                        self.backoff = (self.backoff * 2).min(Duration::from_secs(30));
                        continue;
                    }
                }
            }
            match self.poll().await {
                Step::Deliver(incoming) => return Ok(incoming),
                Step::Continue => {}
                Step::Reconnect(why) => {
                    tracing::warn!(reason = %why, "qq gateway: reconnecting");
                    self.socket = None;
                    tokio::time::sleep(self.backoff).await;
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Outbound: the HTTP API
// ---------------------------------------------------------------------------

pub struct Outbox {
    cfg: Config,
    http: reqwest::Client,
    token: Arc<Token>,
}

impl Outbox {
    fn api(&self) -> &'static str {
        if self.cfg.sandbox { SANDBOX_API } else { LIVE_API }
    }

    async fn post(&self, token: &str, path: &str, body: Value) -> anyhow::Result<()> {
        let url = format!("{}{path}", self.api());
        let response = self
            .http
            .post(&url)
            .header("Authorization", format!("QQBot {token}"))
            .header("X-Union-Appid", &self.cfg.app_id)
            .json(&body)
            .send()
            .await
            .with_context(|| format!("sending to {url}"))?;
        let status = response.status();
        let text = response.text().await.unwrap_or_default();
        if !status.is_success() {
            anyhow::bail!("{status}: {text}");
        }
        Ok(())
    }
}

#[async_trait]
impl OutboxTrait for Outbox {
    async fn send(&self, out: Outgoing) -> anyhow::Result<()> {
        let token = self.token.get().await?;
        let path = match &out.chat {
            ChatId::Group(id) => format!("/v2/groups/{id}/messages"),
            ChatId::Direct(id) => format!("/v2/users/{id}/messages"),
        };
        // Passive first: it threads the answer under the question, which is what
        // a chat looks like. After five minutes (or the sixth reply) the
        // platform refuses that, and a plain message is the only way left to
        // deliver an answer to a long-running turn.
        if let Some((reference, seq)) = &out.reply_to {
            let body = json!({
                "content": out.text,
                "msg_type": 0,
                "msg_id": reference.id,
                "msg_seq": seq,
            });
            match self.post(&token, &path, body).await {
                Ok(()) => return Ok(()),
                Err(e) => {
                    tracing::warn!(error = %e, "passive reply refused; sending it as a new message");
                }
            }
        }
        self.post(&token, &path, json!({"content": out.text, "msg_type": 0}))
            .await
    }
}

// ---------------------------------------------------------------------------
// Decoding one dispatch
// ---------------------------------------------------------------------------

fn parse_dispatch(kind: &str, d: &Value) -> Option<Incoming> {
    let (chat, user) = match kind {
        "GROUP_AT_MESSAGE_CREATE" => (
            ChatId::Group(d["group_openid"].as_str()?.to_string()),
            d["author"]["member_openid"].as_str().unwrap_or_default().to_string(),
        ),
        "C2C_MESSAGE_CREATE" => {
            let openid = d["author"]["user_openid"].as_str()?.to_string();
            (ChatId::Direct(openid.clone()), openid)
        }
        _ => return None,
    };
    let id = d["id"].as_str()?.to_string();
    Some(Incoming {
        chat,
        user,
        text: strip_mentions(d["content"].as_str().unwrap_or_default()),
        reply: MsgRef { id },
    })
}

/// Removes `<@!123456>` / `<@123456>` and the leading space the platform adds.
///
/// Left in, the mention becomes part of the prompt: the agent would see
/// "<@!1234> 跑一下测试" and could reasonably decide the id is significant.
fn strip_mentions(content: &str) -> String {
    let mut out = String::with_capacity(content.len());
    let mut rest = content;
    while let Some(start) = rest.find("<@") {
        out.push_str(&rest[..start]);
        match rest[start..].find('>') {
            Some(end) => rest = &rest[start + end + 1..],
            None => {
                rest = "";
                break;
            }
        }
    }
    out.push_str(rest);
    out.trim().to_string()
}

fn loose_u64(value: &Value) -> Option<u64> {
    value.as_u64().or_else(|| value.as_str().and_then(|s| s.parse().ok()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mentions_are_stripped_from_the_prompt() {
        assert_eq!(strip_mentions("<@!1234567> 跑一下测试"), "跑一下测试");
        assert_eq!(strip_mentions(" <@1234>hi"), "hi");
        assert_eq!(strip_mentions("no mention here"), "no mention here");
        // An unterminated one must not eat the rest of the message.
        assert_eq!(strip_mentions("a <@oops"), "a");
    }

    #[test]
    fn a_group_message_becomes_an_incoming() {
        let d = json!({
            "id": "msg-1",
            "content": " <@!42> hello",
            "group_openid": "GRP123",
            "author": { "member_openid": "USER9" }
        });
        let incoming = parse_dispatch("GROUP_AT_MESSAGE_CREATE", &d).unwrap();
        assert_eq!(incoming.chat, ChatId::Group("GRP123".into()));
        assert_eq!(incoming.user, "USER9");
        assert_eq!(incoming.text, "hello");
        assert_eq!(incoming.reply.id, "msg-1");
        assert_eq!(incoming.chat.session_name("qq"), "qq-g-GRP123");
    }

    #[test]
    fn a_direct_message_becomes_an_incoming() {
        let d = json!({
            "id": "msg-2",
            "content": "hello",
            "author": { "user_openid": "USER1" }
        });
        let incoming = parse_dispatch("C2C_MESSAGE_CREATE", &d).unwrap();
        assert_eq!(incoming.chat, ChatId::Direct("USER1".into()));
        assert_eq!(incoming.user, "USER1");
    }

    /// Anything that is not a message -- reaction, member join, our own send
    /// confirmation -- must not turn into a prompt.
    #[test]
    fn other_events_are_ignored() {
        assert!(parse_dispatch("GROUP_ADD_ROBOT", &json!({"id": "x"})).is_none());
        assert!(parse_dispatch("MESSAGE_CREATE", &json!({"id": "x"})).is_none());
    }

    #[test]
    fn expires_in_parses_either_way() {
        assert_eq!(loose_u64(&json!(7200)), Some(7200));
        assert_eq!(loose_u64(&json!("7200")), Some(7200));
        assert_eq!(loose_u64(&json!(null)), None);
    }
}
