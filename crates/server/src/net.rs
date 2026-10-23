//! UDP listener, connection demultiplexing, and per-client dispatch.
//!
//! One socket serves every client. A datagram whose peer is unknown and whose
//! flags include SYN creates a connection; everything else is routed to the
//! connection that already owns that address. Each connection then runs two
//! concurrent loops: inbound commands and outbound event fan-out.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
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

/// One connected peer. `conn_id` makes release safe: if a client reconnects
/// from the same address before the old connection has finished unwinding, the
/// old task must not delete the new peer's entry.
struct Peer {
    tx: mpsc::Sender<Frame>,
    conn_id: u32,
}

pub async fn serve(cfg: Arc<Config>, d: Arc<Dispatch>, bus: EventBus) -> anyhow::Result<()> {
    let socket = Arc::new(UdpSocket::bind(cfg.bind).await?);
    let local = socket.local_addr()?;
    tracing::info!(%local, "gongga server listening (UDP)");

    let conns: Arc<Mutex<HashMap<SocketAddr, Peer>>> = Arc::new(Mutex::new(HashMap::new()));
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

        // Clone the sender out and drop the lock before awaiting: a slow peer
        // must not stall the loop that every other peer also goes through.
        let known = conns.lock().unwrap().get(&peer).map(|p| p.tx.clone());
        if let Some(tx) = known {
            if tx.send(frame).await.is_err() {
                // The connection task is on its way out and will release the
                // entry itself. Only evict if the address still maps to *this*
                // sender: a reconnect from the same address may have replaced it.
                let mut map = conns.lock().unwrap();
                let stale = map
                    .get(&peer)
                    .map(|p| p.tx.same_channel(&tx))
                    .unwrap_or(false);
                if stale {
                    map.remove(&peer);
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
            // A peer leaves in three ways: `Bye`, a failed send, or the client
            // vanishing. The first two are noticed elsewhere; the third is not
            // -- and a client that reconnects from a fresh local port leaves
            // its old address behind forever. Release it here, unless the same
            // address has already been re-inserted for a newer connection.
            let mut map = conns.lock().unwrap();
            let mine = map.get(&peer).map(|p| p.conn_id == conn_id).unwrap_or(false);
            if mine {
                map.remove(&peer);
                tracing::debug!(%peer, conn_id, "released peer entry");
            }
        });
    }

    // Cancelled: wait for the turns already running to finish writing, however
    // long that takes.
    //
    // There used to be a 30s deadline here, and it was a budget like any other:
    // it decided that a turn had had long enough, and a turn still committing a
    // tool result when it expired lost that work. Nothing needs the deadline
    // either, because the wait has a real end -- `cancel` hands every running
    // turn the same token, and a turn checks it at every point where it waits
    // (model stream, tool call, retry delay) and leaves at its step boundary
    // with its rows already written. So the loop below spins until the count
    // reaches zero, and if a turn ever fails to do that, the count is what says
    // so: it is printed while we wait rather than papered over by an exit.
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
