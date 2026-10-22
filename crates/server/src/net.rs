//! UDP listener, connection demultiplexing, and per-client dispatch.
//!
//! One socket serves every client. A datagram whose peer is unknown and whose
//! flags include SYN creates a connection; everything else is routed to the
//! connection that already owns that address. Each connection then runs two
//! concurrent loops: inbound commands and outbound event fan-out.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use bytes::Bytes;
use protocol::{
    self, ClientMessage, Event, ServerInfo, ServerMessage, decode_client, encode_server,
    frame::{Frame, Flags},
};
use tokio::net::UdpSocket;
use tokio::sync::mpsc;

use crate::bus::EventBus;
use crate::commands::{Dispatch, catalog, dispatch};
use crate::config::Config;

const FRAME_QUEUE: usize = 512;
/// How long a new peer has to present its key before being dropped.
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);
/// How long `cancel` waits for running turns to settle before exiting anyway.
const SHUTDOWN_GRACE: Duration = Duration::from_secs(30);

pub async fn serve(cfg: Arc<Config>, d: Arc<Dispatch>, bus: EventBus) -> anyhow::Result<()> {
    let socket = Arc::new(UdpSocket::bind(cfg.bind).await?);
    let local = socket.local_addr()?;
    tracing::info!(%local, "gongga server listening (UDP)");

    let mut conns: HashMap<SocketAddr, mpsc::Sender<Frame>> = HashMap::new();
    let next_id = AtomicU32::new(1);
    let mut buf = vec![0u8; 65507];

    loop {
        // `cancel` stops acceptance; the loop below then waits for the turns
        // that are already running so nothing is cut off mid-tool-call.
        let received = tokio::select! {
            _ = d.shutdown.cancelled() => None,
            res = socket.recv_from(&mut buf) => Some(res),
        };
        let Some(received) = received else { break };
        let (n, peer) = match received {
            Ok(v) => v,
            Err(e) => {
                // Windows reports WSAECONNRESET on a UDP socket when a previous
                // send reached a port that has since closed. It says nothing about
                // the socket's health, so it must not be treated as a fault --
                // otherwise every client that exits without saying goodbye floods
                // the log.
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

        let known = conns.contains_key(&peer);
        if known {
            if conns.get(&peer).unwrap().send(frame).await.is_err() {
                conns.remove(&peer);
            }
            continue;
        }

        // Unknown peer: only a SYN may open a connection.
        if !frame.flags.contains(Flags::SYN) {
            tracing::debug!(%peer, "ignoring non-SYN datagram from unknown peer");
            continue;
        }

        let (tx, rx) = mpsc::channel::<Frame>(FRAME_QUEUE);
        conns.insert(peer, tx);
        let conn_id = next_id.fetch_add(1, Ordering::Relaxed);
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
        tokio::spawn(async move {
            handle_connection(channel, d, bus, cfg, peer, conn_id).await;
        });
    }

    // Cancelled: give the turns already running a bounded chance to write
    // their results down before the process disappears.
    if d.shutdown.is_cancelled() {
        let deadline = tokio::time::sleep(SHUTDOWN_GRACE);
        tokio::pin!(deadline);
        while d.active.load(Ordering::SeqCst) > 0 {
            tokio::select! {
                _ = &mut deadline => break,
                _ = tokio::time::sleep(Duration::from_millis(50)) => {}
            }
        }
        tracing::info!("server stopped");
    }
    Ok(())
}

/// True for `WSAECONNRESET` (10054), the Windows UDP quirk described above.
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

    // Authenticate before revealing anything: an unauthenticated peer must not
    // even learn the command catalog or the session list.
    if !authenticate(&sink, &mut stream, &cfg.key, peer, conn_id).await {
        return;
    }

    let mut bus_rx = bus.subscribe();
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
            commands: catalog(),
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
                        bus.publish(Event::Cmd {
                            id,
                            name: name.clone(),
                            args: args.clone(),
                        });
                        let outcome = dispatch(&d, &name, &args).await;
                        bus.publish(Event::CmdResult {
                            id,
                            ok: outcome.ok,
                            message: outcome.message,
                            data: outcome.data,
                        });
                    }
                    Ok(ClientMessage::Cancel { session }) => {
                        if let Ok(s) = d.sessions.get(&session) {
                            s.cancel();
                            // Aborting a turn is deliberate, so it is not work
                            // the next start has to pick up. It covers what is
                            // queued behind the running turn as well: those
                            // turns give up once they reach the front.
                            let _ = d.store.clear_pending_tasks(&session);
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
                        if send_server(&sink, &ServerMessage::Event(event)).await.is_err() {
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
        &ServerMessage::Denied { reason: outcome.to_string() },
    )
    .await;
    false
}

/// Length-independent comparison over every byte.
///
/// The key travels in cleartext over UDP, so comparing with `==` would let an
/// attacker time the response to learn how much of a guess is correct.
fn key_matches(given: &str, expected: &str) -> bool {
    let a = given.as_bytes();
    let b = expected.as_bytes();
    let mut acc = a.len() ^ b.len();
    // Fold in every byte of both strings so the work does not stop at the
    // first difference. Short reads are padded with a byte that cannot occur
    // in a hex key, which keeps a wrong-length guess from matching.
    for i in 0..a.len().max(b.len()) {
        let x = a.get(i).copied().unwrap_or(0xff);
        let y = b.get(i).copied().unwrap_or(0xff);
        acc |= (x ^ y) as usize;
    }
    acc == 0
}

async fn send_server(
    sink: &protocol::ChannelSink,
    msg: &ServerMessage,
) -> anyhow::Result<()> {
    let bytes = encode_server(msg)?;
    sink.send(bytes).await.map_err(|e| anyhow::anyhow!("{e}"))
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
