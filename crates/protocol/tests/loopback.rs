//! End-to-end exercise of the reliable channel over a real UDP socket.
//!
//! See `docs/notes/protocol-loopback.md`.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use bytes::Bytes;
// Integration tests see the crate by its published name, not its workspace alias.
use gongga_protocol as protocol;
use protocol::frame::Frame;
use protocol::{ChannelConfig, ChannelSink, ChannelStream};
use tokio::net::UdpSocket;
use tokio::sync::mpsc;

/// Dropping a connection releases its reader task and its socket.
///
/// # Invariant
///
/// Nothing is left running: a client that reconnects -- the TUI redialing a
/// restarted server -- would otherwise leak one reader task and one socket per
/// attempt.
#[tokio::test]
async fn dropping_a_channel_releases_its_reader_task_and_socket() {
    // A peer that never answers, so the only way the reader task can end is
    // being stopped on purpose.
    let silent = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let peer = silent.local_addr().unwrap();

    let socket = Arc::new(UdpSocket::bind("127.0.0.1:0").await.unwrap());
    let channel = protocol::connect(socket.clone(), peer, ChannelConfig::default())
        .await
        .expect("connect starts a channel");
    let (sink, stream) = channel.split();
    assert!(
        Arc::strong_count(&socket) > 1,
        "the channel is supposed to own the socket"
    );

    drop(sink);
    drop(stream);

    // Aborting a task is not instantaneous; what matters is that it happens.
    for _ in 0..100 {
        if Arc::strong_count(&socket) == 1 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(
        Arc::strong_count(&socket),
        1,
        "the reader task and its socket outlived the connection"
    );
    drop(silent);
}

/// Minimal stand-in for the server's listener.
struct Demux {
    socket: Arc<UdpSocket>,
    conns: HashMap<SocketAddr, mpsc::Sender<Frame>>,
    dropped: Arc<AtomicUsize>,
    loss_every: usize,
    seen: usize,
}

impl Demux {
    fn new(socket: Arc<UdpSocket>, loss_every: usize) -> Self {
        Self {
            socket,
            conns: HashMap::new(),
            dropped: Arc::new(AtomicUsize::new(0)),
            loss_every,
            seen: 0,
        }
    }

    /// Runs until `stop` completes, echoing every received message back.
    async fn run(mut self) -> Arc<AtomicUsize> {
        let dropped = self.dropped.clone();
        let mut buf = vec![0u8; 65507];
        loop {
            let (n, peer) = match self.socket.recv_from(&mut buf).await {
                Ok(v) => v,
                Err(_) => break,
            };
            self.seen += 1;
            if self.loss_every > 0 && self.seen % self.loss_every == 0 {
                self.dropped.fetch_add(1, Ordering::Relaxed);
                continue;
            }
            let Ok(frame) = Frame::decode(Bytes::copy_from_slice(&buf[..n])) else {
                continue;
            };
            if let std::collections::hash_map::Entry::Vacant(e) = self.conns.entry(peer) {
                let (tx, rx) = mpsc::channel::<Frame>(1024);
                e.insert(tx);
                let channel =
                    protocol::accept(self.socket.clone(), peer, rx, ChannelConfig::default(), 1);
                tokio::spawn(echo(channel));
            }
            let tx = self.conns.get(&peer).unwrap().clone();
            if tx.send(frame).await.is_err() {
                self.conns.remove(&peer);
            }
        }
        dropped
    }
}

/// Echoes every message back to the client, reversed so it is unmistakable.
async fn echo(channel: protocol::Channel) {
    let (sink, mut stream) = channel.split();
    while let Some(msg) = stream.recv().await {
        let mut echoed = msg.clone();
        echoed.reverse();
        if sink.send(echoed).await.is_err() {
            break;
        }
    }
}

async fn pair(loss_every: usize) -> (ChannelSink, ChannelStream, Arc<AtomicUsize>, SocketAddr) {
    let server = Arc::new(UdpSocket::bind("127.0.0.1:0").await.unwrap());
    let server_addr = server.local_addr().unwrap();
    let demux = Demux::new(server, loss_every);
    let dropped = demux.dropped.clone();
    tokio::spawn(demux.run());

    let client = Arc::new(UdpSocket::bind("127.0.0.1:0").await.unwrap());
    let channel = protocol::connect(client, server_addr, ChannelConfig::default())
        .await
        .expect("handshake");
    let (sink, stream) = channel.split();
    (sink, stream, dropped, server_addr)
}

#[tokio::test]
async fn round_trips_a_message_that_needs_many_fragments() {
    let (sink, mut stream, _dropped, _addr) = pair(0).await;

    // 200 KiB is ~170 fragments at the default MTU, covering fragmentation, windowing
    // and in-order reassembly.
    let payload: Vec<u8> = (0..200 * 1024).map(|i| (i % 251) as u8).collect();
    sink.send(payload.clone()).await.unwrap();

    let echoed = tokio::time::timeout(Duration::from_secs(20), stream.recv())
        .await
        .expect("echo within timeout")
        .expect("channel open");
    let mut expected = payload.clone();
    expected.reverse();
    assert_eq!(echoed, expected);
}

#[tokio::test]
async fn recovers_from_dropped_datagrams() {
    let (sink, mut stream, dropped, _addr) = pair(4).await;

    let payload: Vec<u8> = (0..40 * 1024).map(|i| (i % 97) as u8).collect();
    sink.send(payload.clone()).await.unwrap();

    let echoed = tokio::time::timeout(Duration::from_secs(30), stream.recv())
        .await
        .expect("echo arrives despite loss")
        .expect("channel open");

    let mut expected = payload.clone();
    expected.reverse();
    assert_eq!(echoed, expected);
    assert!(
        dropped.load(Ordering::Relaxed) > 0,
        "the test is only meaningful if datagrams were actually dropped"
    );
}

/// A consumer that stops reading must not stop the link.
/// See `docs/notes/protocol-loopback.md`.
///
/// # Invariant
///
/// The replies queue up while nobody reads, and every one of them still arrives,
/// in order.
#[tokio::test]
async fn a_paused_consumer_does_not_stall_the_link() {
    let (sink, mut stream, _dropped, _addr) = pair(0).await;
    const TOTAL: usize = 400;
    // The producer holds its sink until released: dropping a half closes the link.
    let (release, held) = tokio::sync::oneshot::channel::<()>();
    let producer = tokio::spawn(async move {
        for i in 0..TOTAL {
            if sink
                .send(format!("message-{i}").into_bytes())
                .await
                .is_err()
            {
                return i;
            }
        }
        let _ = held.await;
        TOTAL
    });

    // Long enough that far more replies are waiting than any queue used to
    // hold in one go.
    tokio::time::sleep(Duration::from_millis(750)).await;

    for i in 0..TOTAL {
        let echoed = tokio::time::timeout(Duration::from_secs(20), stream.recv())
            .await
            .unwrap_or_else(|_| panic!("no message {i} within 20s: the link stalled"))
            .expect("channel open");
        let mut expected = format!("message-{i}").into_bytes();
        expected.reverse();
        assert_eq!(echoed, expected, "message {i} arrived out of order");
    }
    let _ = release.send(());
    assert_eq!(producer.await.unwrap(), TOTAL, "the producer gave up early");
}

#[tokio::test]
async fn several_messages_arrive_in_order() {
    let (sink, mut stream, _dropped, _addr) = pair(0).await;

    for i in 0..8u8 {
        let payload = vec![i; 5000];
        sink.send(payload).await.unwrap();
    }

    for i in 0..8u8 {
        let echoed = tokio::time::timeout(Duration::from_secs(20), stream.recv())
            .await
            .expect("echo")
            .expect("channel open");
        // Reversing a uniform buffer is a no-op, so check the contents directly.
        assert_eq!(echoed, vec![i; 5000], "message {i} arrived out of order");
    }
}
