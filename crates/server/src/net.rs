//! UDP listener, connection demultiplexing, and per-client dispatch.
//!
//! See `docs/notes/server-net.md`.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use bytes::Bytes;
use protocol::{
    self, Args, ChannelError, ClientMessage, Event, ServerInfo, ServerMessage, decode_client,
    encode_server,
    frame::{Flags, Frame},
};
use tokio::net::UdpSocket;
use tokio::sync::mpsc;

use crate::bus::EventBus;
use crate::commands::{Dispatch, dispatch};
use crate::config::Config;

const FRAME_QUEUE: usize = 512;
/// How long a new peer has to present its key before being dropped.
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);

/// One connected peer; `conn_id` keeps release from evicting a newer connection.
struct Peer {
    tx: mpsc::Sender<Frame>,
    conn_id: u32,
}

/// One command of one connection, either running or waiting its turn.
struct Queued {
    id: u64,
    name: String,
    args: Args,
}

pub async fn serve(cfg: Arc<Config>, d: Arc<Dispatch>, bus: EventBus) -> anyhow::Result<()> {
    let socket = Arc::new(UdpSocket::bind(cfg.bind).await?);
    let local = socket.local_addr()?;
    tracing::info!(%local, "gongga server listening (UDP)");

    let conns: Arc<Mutex<HashMap<SocketAddr, Peer>>> = Arc::new(Mutex::new(HashMap::new()));
    let next_id = AtomicU32::new(1);
    let mut buf = vec![0u8; 65507];

    loop {
        // `cancel` stops acceptance; running turns finish before the loop exits.
        let received = tokio::select! {
            _ = d.shutdown.cancelled() => None,
            res = socket.recv_from(&mut buf) => Some(res),
        };
        let Some(received) = received else { break };
        let (n, peer) = match received {
            Ok(v) => v,
            Err(e) => {
                // Windows says WSAECONNRESET when a peer's port has closed; not a
                // fault here, or every rude exit floods the log.
                if is_connreset(&e) {
                    tracing::trace!(error = %e, "ignored ICMP port-unreachable");
                } else {
                    tracing::warn!(error = %e, "recv_from failed");
                }
                continue;
            }
        };
        let frame = match Frame::decode(Bytes::copy_from_slice(&buf[..n])) {
            Ok(f) => f,
            Err(e) => {
                tracing::debug!(%peer, error = %e, "dropping malformed datagram");
                continue;
            }
        };

        // Clone the sender, drop the lock: a slow peer must not stall the loop.
        // A full inbox is ARQ's to retry.
        let known = conns.lock().unwrap().get(&peer).map(|p| p.tx.clone());
        if let Some(tx) = known {
            match tx.try_send(frame) {
                Ok(()) | Err(mpsc::error::TrySendError::Full(_)) => {}
                Err(mpsc::error::TrySendError::Closed(_)) => {
                    // Evict only if the address still maps to this sender, not a reconnect.
                    let mut map = conns.lock().unwrap();
                    let stale = map
                        .get(&peer)
                        .map(|p| p.tx.same_channel(&tx))
                        .unwrap_or(false);
                    if stale {
                        map.remove(&peer);
                    }
                }
            }
            continue;
        }

        // Unknown peer: only a SYN may open a connection.
        if !frame.flags.contains(Flags::SYN) {
            tracing::debug!(%peer, "ignoring non-SYN datagram from unknown peer");
            continue;
        }

        let (tx, rx) = mpsc::channel::<Frame>(FRAME_QUEUE);
        // The opening SYN is also this connection's first input: without it no
        // SYN-ACK goes out and the client redials.
        if tx.try_send(frame).is_err() {
            continue;
        }
        let conn_id = next_id.fetch_add(1, Ordering::Relaxed);
        conns.lock().unwrap().insert(peer, Peer { tx, conn_id });
        let channel = protocol::accept(
            socket.clone(),
            peer,
            rx,
            cfg.transport.clone().into(),
            conn_id,
        );
        tracing::info!(%peer, conn_id, "client connected");

        let d = d.clone();
        let bus = bus.clone();
        let cfg = cfg.clone();
        let conns = conns.clone();
        tokio::spawn(async move {
            handle_connection(channel, d, bus, cfg, peer, conn_id).await;
            // Release the peer here: a client that merely vanishes is noticed
            // nowhere else. See `docs/notes/server-net.md`.
            let mut map = conns.lock().unwrap();
            let mine = map
                .get(&peer)
                .map(|p| p.conn_id == conn_id)
                .unwrap_or(false);
            if mine {
                map.remove(&peer);
                tracing::debug!(%peer, conn_id, "released peer entry");
            }
        });
    }

    // Cancelled: wait for running turns to finish, with no deadline at all.
    if d.shutdown.is_cancelled() {
        let mut ticks: u64 = 0;
        while d.active.load(Ordering::SeqCst) > 0 {
            tokio::time::sleep(Duration::from_millis(50)).await;
            ticks += 1;
            // Every ten seconds, name the fact that something is still holding
            // the process open. Observability, not a cutoff.
            if ticks % 200 == 0 {
                tracing::warn!(
                    active = d.active.load(Ordering::SeqCst),
                    waited = ?Duration::from_millis(ticks * 50),
                    "still waiting for running turn(s) to stop"
                );
            }
        }
        tracing::info!("server stopped");
    }
    Ok(())
}

/// True for `WSAECONNRESET` (10054); see `docs/notes/server-net.md`.
fn is_connreset(e: &std::io::Error) -> bool {
    cfg!(windows) && e.raw_os_error() == Some(10054)
}

async fn handle_connection(
    channel: protocol::Channel,
    d: Arc<Dispatch>,
    bus: EventBus,
    cfg: Arc<Config>,
    peer: SocketAddr,
    conn_id: u32,
) {
    let (sink, mut stream) = channel.split();

    // Authenticate first: an unauthenticated peer must not see the catalog.
    if !authenticate(&sink, &mut stream, &cfg.key, peer, conn_id).await {
        return;
    }

    let mut bus_rx = bus.subscribe();
    // Commands run elsewhere: one must not hold up the events owed here.
    // See `docs/notes/server-net.md`.
    let (commands, queued) = mpsc::unbounded_channel::<Queued>();
    tokio::spawn(run_commands(d.clone(), queued, sink.clone()));
    if let Err(e) = send_server(
        &sink,
        &ServerMessage::Hello {
            protocol: protocol::PROTOCOL_VERSION,
            server: ServerInfo {
                version: env!("CARGO_PKG_VERSION").to_string(),
                pid: std::process::id(),
                started_at: crate::store::now_millis(),
                cwd: std::env::current_dir()
                    .map(|p| p.to_string_lossy().to_string())
                    .unwrap_or_default(),
                model: cfg.llm.model.clone(),
            },
            // The one list: built-ins, client commands, and a script's.
            commands: d.catalog().await,
            sessions: d
                .sessions
                .list()
                .unwrap_or_default()
                .iter()
                .map(|r| protocol::SessionSummary {
                    name: r.name.clone(),
                    status: r.status,
                    created_at: r.created_at,
                    updated_at: r.updated_at,
                    messages: r.messages,
                    cwd: r.cwd.clone(),
                })
                .collect(),
        },
    )
    .await
    {
        tracing::warn!(%peer, error = %e, "could not greet client");
        return;
    }

    loop {
        tokio::select! {
            incoming = stream.recv() => {
                let Some(bytes) = incoming else { break };
                match decode_client(&bytes) {
                    Ok(ClientMessage::Hello { client, .. }) => {
                        tracing::info!(%peer, conn_id, label = %client.label, "client ready");
                    }
                    Ok(ClientMessage::Command { id, name, args }) => {
                        // Cmd is this request's local echo, not a global event.
                        let echo = Event::Cmd {
                            id,
                            name: name.clone(),
                            args: args.clone(),
                        };
                        if send_event(&sink, echo).await.is_err() {
                            break;
                        }
                        if commands
                            .send(Queued { id, name, args })
                            .is_err()
                        {
                            // Nobody left to run it, so nobody left to answer.
                            break;
                        }
                    }
                    Ok(ClientMessage::Bye) => break,
                    Err(e) => {
                        tracing::warn!(%peer, error = %e, "dropping unparsable message");
                    }
                }
            }
            event = bus_rx.recv() => {
                match event {
                    Ok(event) => {
                        if send_event(&sink, event).await.is_err() {
                            break;
                        }
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                        tracing::debug!(%peer, skipped = n, "client fell behind");
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                }
            }
        }
    }

    tracing::info!(%peer, conn_id, "client disconnected");
}

/// Runs one connection's commands, one at a time and in the order they arrived.
/// See `docs/notes/server-net.md`.
async fn run_commands(
    d: Arc<Dispatch>,
    mut queued: mpsc::UnboundedReceiver<Queued>,
    sink: protocol::ChannelSink,
) {
    while let Some(command) = queued.recv().await {
        let outcome = dispatch(&d, &command.name, &command.args).await;
        let result = Event::CmdResult {
            id: command.id,
            ok: outcome.ok,
            message: outcome.message,
            data: outcome.data,
        };
        if let Err(e) = send_event(&sink, result).await {
            tracing::debug!(id = command.id, error = %e, "command reply connection closed");
            break;
        }
    }
}

/// Waits for the client's `Hello` and checks its key. False means "closed".
async fn authenticate(
    sink: &protocol::ChannelSink,
    stream: &mut protocol::ChannelStream,
    expected: &str,
    peer: SocketAddr,
    conn_id: u32,
) -> bool {
    let greeting = tokio::time::timeout(HANDSHAKE_TIMEOUT, stream.recv()).await;
    let outcome = match greeting {
        Ok(Some(bytes)) => match decode_client(&bytes) {
            Ok(ClientMessage::Hello { client, key }) => {
                if key_matches(&key, expected) {
                    tracing::info!(%peer, conn_id, label = %client.label, "client connected");
                    return true;
                }
                tracing::warn!(%peer, "rejected client: wrong key");
                "wrong key"
            }
            Ok(_) => "expected a Hello first",
            Err(_) => "unparsable handshake",
        },
        Ok(None) => return false,
        Err(_) => {
            tracing::debug!(%peer, "handshake timed out");
            return false;
        }
    };
    let _ = send_server(
        sink,
        &ServerMessage::Denied {
            reason: outcome.to_string(),
        },
    )
    .await;
    false
}

/// Length-independent comparison over every byte, so timing leaks no prefix.
fn key_matches(given: &str, expected: &str) -> bool {
    let a = given.as_bytes();
    let b = expected.as_bytes();
    let mut acc = a.len() ^ b.len();
    // No early exit, and a pad byte no hex key contains: both lengths are folded in.
    for i in 0..a.len().max(b.len()) {
        let x = a.get(i).copied().unwrap_or(0xff);
        let y = b.get(i).copied().unwrap_or(0xff);
        acc |= (x ^ y) as usize;
    }
    acc == 0
}

async fn send_server(sink: &protocol::ChannelSink, msg: &ServerMessage) -> anyhow::Result<()> {
    let bytes = encode_server(msg)?;
    sink.send(bytes).await.map_err(|e| anyhow::anyhow!("{e}"))
}

/// Sends one event, reporting a reply too large to carry instead of dying.
///
/// # Invariant
///
/// A size refusal keeps the connection: the caller asked a question this channel
/// cannot answer, and the answer to that is a sentence, not a dropped link.
async fn send_event(sink: &protocol::ChannelSink, event: Event) -> anyhow::Result<()> {
    let bytes = encode_server(&ServerMessage::Event(event.clone()))?;
    match sink.send(bytes).await {
        Ok(()) => Ok(()),
        Err(ChannelError::MessageTooLarge { len, limit }) => {
            // Only a command result has a caller to tell; the rest has nowhere to go.
            let Event::CmdResult { id, .. } = event else {
                tracing::warn!(len, limit, "dropped an event too large to send");
                return Ok(());
            };
            let why = format!(
                "the reply was {len} bytes, over the {limit} bytes one message carries; \
                 ask for fewer rows with --range, e.g. `--range -500:`"
            );
            tracing::warn!(id, len, limit, "reply too large to send");
            let small = ServerMessage::Event(Event::CmdResult {
                id,
                ok: false,
                message: why,
                data: None,
            });
            sink.send(encode_server(&small)?)
                .await
                .map_err(|e| anyhow::anyhow!("{e}"))
        }
        Err(e) => Err(anyhow::anyhow!("{e}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_match_only_exactly() {
        assert!(key_matches("abc", "abc"));
        assert!(!key_matches("abc", "abd"));
        assert!(!key_matches("abc", "ab"));
        assert!(!key_matches("ab", "abc"));
        assert!(!key_matches("", "abc"));
        assert!(key_matches("", ""));
    }
}
