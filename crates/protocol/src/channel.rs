//! A small reliable, ordered message channel over UDP.
//!
//! See `docs/notes/protocol-channel.md`.

use std::collections::{BTreeMap, VecDeque};
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use bytes::{Bytes, BytesMut};
use tokio::net::UdpSocket;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio::time::interval;

use crate::frame::{DEFAULT_MTU, Flags, Frame, FrameError, HEADER_LEN};

/// Channel tuning; the defaults assume one CLI-to-server link with few-ms RTTs.
#[derive(Debug, Clone)]
pub struct ChannelConfig {
    /// Maximum datagram size, including the 26-byte header.
    pub mtu: usize,
    /// Maximum number of packets in flight.
    pub window: u16,
    /// How many out-of-order packets the receive side will hold.
    pub recv_buffer: usize,
    /// Upper bound on a reassembled message, guarding against memory blowups.
    pub max_message_len: usize,
    /// Idle gap after which a keepalive is emitted.
    pub keepalive: Duration,
    /// How long without any inbound packet before the channel is declared dead.
    pub idle_timeout: Duration,
    /// Consecutive retransmissions of a packet before giving up.
    pub max_retransmits: u32,
    /// RTO bounds and starting point.
    pub initial_rto: Duration,
    pub min_rto: Duration,
    pub max_rto: Duration,
    /// Delay before emitting a standalone ACK, in the hope of piggybacking it.
    pub ack_delay: Duration,
    /// Timer granularity.
    pub tick: Duration,
}

impl Default for ChannelConfig {
    fn default() -> Self {
        Self {
            mtu: DEFAULT_MTU,
            window: 64,
            recv_buffer: 256,
            max_message_len: 8 * 1024 * 1024,
            keepalive: Duration::from_secs(2),
            idle_timeout: Duration::from_secs(60),
            max_retransmits: 12,
            initial_rto: Duration::from_millis(200),
            min_rto: Duration::from_millis(50),
            max_rto: Duration::from_secs(5),
            ack_delay: Duration::from_millis(20),
            tick: Duration::from_millis(10),
        }
    }
}

impl ChannelConfig {
    pub fn payload_capacity(&self) -> usize {
        self.mtu.saturating_sub(HEADER_LEN).max(1)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    Client,
    Server,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ConnState {
    Handshaking,
    Established,
    Closing,
}

#[derive(Debug, thiserror::Error)]
pub enum ChannelError {
    #[error("channel closed")]
    Closed,
    #[error("peer stopped responding after {0} retransmissions")]
    Timeout(u32),
    #[error("peer reset the channel")]
    Reset,
    #[error("malformed frame: {0}")]
    Frame(#[from] FrameError),
    #[error("socket error: {0}")]
    Io(#[from] std::io::Error),
    #[error("message of {len} bytes exceeds the {limit} byte limit")]
    MessageTooLarge { len: usize, limit: usize },
}

/// What a channel's receive side shares with its handles: the backlog and the reader task.
struct Inbound {
    /// Messages delivered but not taken; this is the receive window we advertise.
    pending: AtomicUsize,
    /// Aborts the reader when the last handle drops; `None` when a demux owns the socket.
    _reader: Option<ReaderAbort>,
}

impl Inbound {
    fn new(reader: Option<ReaderAbort>) -> Arc<Self> {
        Arc::new(Self {
            pending: AtomicUsize::new(0),
            _reader: reader,
        })
    }

    /// How many delivered messages the caller has not collected.
    fn backlog(&self) -> usize {
        self.pending.load(Ordering::Relaxed)
    }

    /// Awaits the next message and accounts for it.
    async fn recv(&self, inbound: &mut mpsc::UnboundedReceiver<Vec<u8>>) -> Option<Vec<u8>> {
        let message = inbound.recv().await?;
        // Saturating: a cancel between arrival and here must not wrap to `usize::MAX`.
        let _ = self
            .pending
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |n| n.checked_sub(1));
        Some(message)
    }
}

/// Stops the reader task when the last handle goes away. See
/// `docs/notes/protocol-channel.md`.
struct ReaderAbort(JoinHandle<()>);

impl Drop for ReaderAbort {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// A handle to a running channel; `send` fails and `recv` yields `None` once it stops.
pub struct Channel {
    outbound: mpsc::Sender<Vec<u8>>,
    inbound: mpsc::UnboundedReceiver<Vec<u8>>,
    shared: Arc<Inbound>,
    peer: SocketAddr,
    conn_id: Arc<std::sync::Mutex<u32>>,
}

impl Channel {
    pub fn peer(&self) -> SocketAddr {
        self.peer
    }

    /// The connection id assigned during the handshake. `0` until it completes.
    pub fn conn_id(&self) -> u32 {
        *self.conn_id.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub async fn send(&self, message: Vec<u8>) -> Result<(), ChannelError> {
        self.outbound
            .send(message)
            .await
            .map_err(|_| ChannelError::Closed)
    }

    /// Awaits the next complete message, or `None` once the channel is finished.
    pub async fn recv(&mut self) -> Option<Vec<u8>> {
        self.shared.recv(&mut self.inbound).await
    }

    /// Splits the handle so one task sends while another receives; both halves hold the link.
    pub fn split(self) -> (ChannelSink, ChannelStream) {
        (
            ChannelSink {
                outbound: self.outbound,
                peer: self.peer,
                conn_id: self.conn_id,
                _shared: self.shared.clone(),
            },
            ChannelStream {
                inbound: self.inbound,
                shared: self.shared,
            },
        )
    }
}

/// The sending half of a [`Channel`].
#[derive(Clone)]
pub struct ChannelSink {
    outbound: mpsc::Sender<Vec<u8>>,
    peer: SocketAddr,
    conn_id: Arc<std::sync::Mutex<u32>>,
    /// Kept only to hold the reader task open: a sink alone is still a link to come back to.
    _shared: Arc<Inbound>,
}

impl ChannelSink {
    pub fn peer(&self) -> SocketAddr {
        self.peer
    }

    pub fn conn_id(&self) -> u32 {
        *self.conn_id.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub async fn send(&self, message: Vec<u8>) -> Result<(), ChannelError> {
        self.outbound
            .send(message)
            .await
            .map_err(|_| ChannelError::Closed)
    }
}

/// The receiving half of a [`Channel`].
pub struct ChannelStream {
    inbound: mpsc::UnboundedReceiver<Vec<u8>>,
    shared: Arc<Inbound>,
}

impl ChannelStream {
    pub async fn recv(&mut self) -> Option<Vec<u8>> {
        self.shared.recv(&mut self.inbound).await
    }
}

/// A packet that has been handed to the socket but not yet acknowledged.
struct InFlight {
    payload: Bytes,
    msg_id: u32,
    frag_idx: u16,
    frag_cnt: u16,
    sent_at: Instant,
    retransmits: u32,
    /// Set once retransmitted, so Karn's algorithm skips it for RTT sampling.
    retransmitted: bool,
}

/// A packet waiting for window space.
struct Queued {
    payload: Bytes,
    msg_id: u32,
    frag_idx: u16,
    frag_cnt: u16,
}

struct Reassembly {
    msg_id: u32,
    frag_cnt: u16,
    next_idx: u16,
    buf: Vec<u8>,
}

struct ChannelCore {
    cfg: ChannelConfig,
    conn_id: u32,
    state: ConnState,

    next_seq: u32,
    send_base: u32,
    in_flight: BTreeMap<u32, InFlight>,
    queued: VecDeque<Queued>,
    next_msg_id: u32,

    srtt: Option<Duration>,
    rttvar: Duration,
    rto: Duration,
    backoff: u32,

    recv_expected: u32,
    recv_buf: BTreeMap<u32, Frame>,
    reassembly: Option<Reassembly>,

    highest_ack: u32,
    dup_acks: u32,
    ack_due: Option<Instant>,
    last_recv: Instant,
    last_send: Instant,
    peer_window: u16,
    /// The window to advertise: `cfg.window` minus the uncollected backlog, recomputed each pass.
    recv_window: u16,
    /// Set by ACK processing when three duplicate ACKs warrant a fast retransmit.
    fast_retransmit: bool,
    /// Set when a bare SYN arrives and we owe the peer a SYN-ACK.
    pending_syn_ack: bool,
}

impl ChannelCore {
    fn new(cfg: ChannelConfig, role: Role, conn_id: u32) -> Self {
        let rto = cfg.initial_rto;
        let window = cfg.window;
        Self {
            cfg,
            conn_id,
            state: match role {
                Role::Client => ConnState::Handshaking,
                Role::Server => ConnState::Established,
            },
            next_seq: 1,
            send_base: 1,
            in_flight: BTreeMap::new(),
            queued: VecDeque::new(),
            next_msg_id: 1,
            srtt: None,
            rttvar: rto / 2,
            rto,
            backoff: 0,
            recv_expected: 1,
            recv_buf: BTreeMap::new(),
            reassembly: None,
            highest_ack: 0,
            dup_acks: 0,
            ack_due: None,
            last_recv: Instant::now(),
            last_send: Instant::now(),
            peer_window: u16::MAX,
            recv_window: window,
            fast_retransmit: false,
            pending_syn_ack: false,
        }
    }

    fn window(&self) -> usize {
        self.cfg.window.min(self.peer_window).max(1) as usize
    }

    /// What to tell the peer our receive window is, given a delivery backlog.
    ///
    /// # Invariant
    ///
    /// Never zero: a zero window is a deadlock signal a peer may honour forever,
    /// and the point is to slow a producer down while the I/O task keeps running,
    /// not to stop it. One packet of room still lets the link make progress, and
    /// every message the caller takes back gives the full window back.
    fn advertised_window(&self, backlog: usize) -> u16 {
        let backlog = u16::try_from(backlog).unwrap_or(u16::MAX);
        self.cfg.window.saturating_sub(backlog).max(1)
    }

    /// Splits a message into queued fragments.
    fn enqueue(&mut self, message: Vec<u8>) -> Result<(), ChannelError> {
        if message.len() > self.cfg.max_message_len {
            return Err(ChannelError::MessageTooLarge {
                len: message.len(),
                limit: self.cfg.max_message_len,
            });
        }
        let msg_id = self.next_msg_id;
        self.next_msg_id = self.next_msg_id.wrapping_add(1);

        let cap = self.cfg.payload_capacity();
        let payload = Bytes::from(message);
        let chunks: Vec<Bytes> = if payload.is_empty() {
            vec![Bytes::new()]
        } else {
            payload.chunks(cap).map(|c| payload.slice_ref(c)).collect()
        };
        let frag_cnt = u16::try_from(chunks.len()).map_err(|_| ChannelError::MessageTooLarge {
            len: payload.len(),
            limit: self.cfg.max_message_len,
        })?;

        for (i, chunk) in chunks.into_iter().enumerate() {
            self.queued.push_back(Queued {
                payload: chunk,
                msg_id,
                frag_idx: i as u16,
                frag_cnt,
            });
        }
        Ok(())
    }

    fn build_data(&self, q: &Queued, seq: u32) -> Frame {
        Frame::data(
            self.conn_id,
            seq,
            self.recv_expected,
            q.msg_id,
            q.frag_idx,
            q.frag_cnt,
            self.recv_window,
            q.payload.clone(),
        )
    }

    /// Emits as many queued packets as the window allows.
    async fn flush(&mut self, io: &Sender) -> Result<(), ChannelError> {
        if self.state == ConnState::Handshaking {
            return Ok(());
        }
        while self.in_flight.len() < self.window() {
            let Some(q) = self.queued.pop_front() else {
                break;
            };
            let seq = self.next_seq;
            self.next_seq = self.next_seq.wrapping_add(1);
            let frame = self.build_data(&q, seq);
            io.send(&frame).await?;
            self.last_send = Instant::now();
            self.ack_due = None;
            self.in_flight.insert(
                seq,
                InFlight {
                    payload: q.payload,
                    msg_id: q.msg_id,
                    frag_idx: q.frag_idx,
                    frag_cnt: q.frag_cnt,
                    sent_at: Instant::now(),
                    retransmits: 0,
                    retransmitted: false,
                },
            );
        }
        Ok(())
    }

    async fn send_ack(&mut self, io: &Sender) -> Result<(), ChannelError> {
        let mut frame = Frame::ack(self.conn_id, self.recv_expected, self.recv_window);
        if self.state == ConnState::Handshaking {
            frame.flags = frame.flags.union(Flags::SYN);
        }
        io.send(&frame).await?;
        self.last_send = Instant::now();
        self.ack_due = None;
        Ok(())
    }

    fn on_ack(&mut self, ack: u32, window: u16) {
        self.peer_window = window;
        if seq_le(ack, self.highest_ack) && self.highest_ack != 0 {
            if ack == self.highest_ack {
                self.dup_acks += 1;
            }
        } else {
            self.highest_ack = ack;
            self.dup_acks = 0;
        }

        // Retire everything the peer has seen.
        let mut advanced = false;
        let retired: Vec<u32> = self
            .in_flight
            .keys()
            .copied()
            .filter(|s| seq_lt(*s, ack))
            .collect();
        for seq in retired {
            if let Some(pkt) = self.in_flight.remove(&seq) {
                advanced = true;
                if !pkt.retransmitted {
                    self.sample_rtt(pkt.sent_at.elapsed());
                }
            }
        }
        if advanced {
            self.send_base = ack;
            self.backoff = 0;
        }

        if self.dup_acks >= 3 {
            self.dup_acks = 0;
            self.fast_retransmit = true;
        }
    }

    fn sample_rtt(&mut self, rtt: Duration) {
        let rtt_ms = rtt.as_secs_f64() * 1000.0;
        let (srtt_ms, rttvar_ms) = match self.srtt {
            None => (rtt_ms, rtt_ms / 2.0),
            Some(srtt) => {
                let srtt_ms = srtt.as_secs_f64() * 1000.0;
                let rttvar_ms = self.rttvar.as_secs_f64() * 1000.0;
                let delta = (srtt_ms - rtt_ms).abs();
                (
                    srtt_ms * 7.0 / 8.0 + rtt_ms / 8.0,
                    rttvar_ms * 3.0 / 4.0 + delta / 4.0,
                )
            }
        };
        self.srtt = Some(Duration::from_secs_f64(srtt_ms / 1000.0));
        self.rttvar = Duration::from_secs_f64(rttvar_ms / 1000.0);
        self.set_rto(Duration::from_secs_f64(
            (srtt_ms + 4.0 * rttvar_ms) / 1000.0,
        ));
    }

    fn set_rto(&mut self, rto: Duration) {
        let clamped = rto.clamp(self.cfg.min_rto, self.cfg.max_rto);
        self.rto = clamped;
    }

    fn effective_rto(&self) -> Duration {
        let doubled = self.rto * 2u32.saturating_pow(self.backoff.min(8));
        doubled.clamp(self.cfg.min_rto, self.cfg.max_rto)
    }

    /// Per-packet RTO; per-channel backoff would stall all other packets on one loss.
    fn packet_rto(&self, retransmits: u32) -> Duration {
        let factor = 1u32 << retransmits.min(5);
        (self.rto * factor).clamp(self.cfg.min_rto, self.cfg.max_rto)
    }

    /// Delivers a data frame that has arrived in sequence order.
    fn accept(&mut self, frame: Frame, out: &mut Vec<Vec<u8>>) -> Result<(), ChannelError> {
        let needs_new = match &self.reassembly {
            None => true,
            Some(r) => r.msg_id != frame.msg_id,
        };
        if needs_new {
            if frame.frag_idx != 0 {
                // Fragments must arrive contiguously; anything else is a bug or
                // an attack, and either way we cannot recover.
                return Err(ChannelError::Frame(FrameError::BadFragment {
                    frag_idx: frame.frag_idx,
                    frag_cnt: frame.frag_cnt,
                }));
            }
            self.reassembly = Some(Reassembly {
                msg_id: frame.msg_id,
                frag_cnt: frame.frag_cnt,
                next_idx: 0,
                buf: Vec::new(),
            });
        }
        let done = {
            let r = self.reassembly.as_mut().unwrap();
            if frame.frag_idx != r.next_idx {
                return Err(ChannelError::Frame(FrameError::BadFragment {
                    frag_idx: frame.frag_idx,
                    frag_cnt: r.frag_cnt,
                }));
            }
            r.buf.extend_from_slice(&frame.payload);
            r.next_idx += 1;
            if r.buf.len() > self.cfg.max_message_len {
                return Err(ChannelError::MessageTooLarge {
                    len: r.buf.len(),
                    limit: self.cfg.max_message_len,
                });
            }
            r.next_idx == r.frag_cnt
        };
        if done {
            let mut r = self.reassembly.take().unwrap();
            out.push(std::mem::take(&mut r.buf));
        }
        Ok(())
    }

    fn on_frame(&mut self, frame: Frame, out: &mut Vec<Vec<u8>>) -> Result<(), ChannelError> {
        self.last_recv = Instant::now();
        if frame.flags.contains(Flags::RST) {
            self.state = ConnState::Closing;
            return Err(ChannelError::Reset);
        }
        // FIN: the peer is done; stop reading instead of waiting out idle_timeout.
        if frame.flags.contains(Flags::FIN) {
            self.state = ConnState::Closing;
            return Err(ChannelError::Closed);
        }

        if frame.flags.contains(Flags::SYN) && !frame.flags.contains(Flags::ACK) {
            // Answer any SYN, including repeats of one we already answered.
            self.pending_syn_ack = true;
        }
        if frame.flags.contains(Flags::SYN)
            && frame.flags.contains(Flags::ACK)
            && self.state == ConnState::Handshaking
        {
            self.conn_id = frame.conn_id;
            self.state = ConnState::Established;
            self.backoff = 0;
        }
        if frame.window > 0 {
            self.peer_window = frame.window;
        }
        if frame.flags.contains(Flags::ACK) && self.state != ConnState::Handshaking {
            self.on_ack(frame.ack, frame.window);
        }

        if !frame.is_data() {
            return Ok(());
        }

        if seq_lt(frame.seq, self.recv_expected) {
            // Duplicate; re-ACK promptly so the sender can retire it.
            self.ack_due = Some(Instant::now());
            return Ok(());
        }
        if frame.seq == self.recv_expected {
            self.accept(frame, out)?;
            self.recv_expected = self.recv_expected.wrapping_add(1);
            while let Some(next) = self.recv_buf.remove(&self.recv_expected) {
                self.accept(next, out)?;
                self.recv_expected = self.recv_expected.wrapping_add(1);
            }
            if self.ack_due.is_none() {
                self.ack_due = Some(Instant::now() + self.cfg.ack_delay);
            }
        } else {
            if self.recv_buf.len() < self.cfg.recv_buffer {
                self.recv_buf.insert(frame.seq, frame);
            }
            // Out-of-order arrival: tell the sender right away.
            self.ack_due = Some(Instant::now());
        }
        Ok(())
    }
}

/// The sender is immutable and shared; a demultiplexer fills the per-peer inbox.
struct Sender {
    socket: Arc<UdpSocket>,
    peer: SocketAddr,
}

impl Sender {
    async fn send(&self, frame: &Frame) -> Result<(), ChannelError> {
        let mut buf = BytesMut::new();
        frame.encode(&mut buf);
        self.socket.send_to(&buf, self.peer).await?;
        Ok(())
    }
}

/// Spawns a client-side channel and performs the SYN/SYN-ACK handshake.
///
/// # Invariant
///
/// `socket` must already be bound locally and is left unconnected so the caller
/// can reuse it; frames from peers other than `peer` are ignored.
pub async fn connect(
    socket: Arc<UdpSocket>,
    peer: SocketAddr,
    cfg: ChannelConfig,
) -> Result<Channel, ChannelError> {
    let (tx, rx) = mpsc::channel::<Frame>(512);
    let reader = socket.clone();
    let reader_task = tokio::spawn(async move {
        let mut buf = vec![0u8; 65507];
        loop {
            match reader.recv_from(&mut buf).await {
                Ok((n, addr)) if addr == peer => {
                    match Frame::decode(Bytes::copy_from_slice(&buf[..n])) {
                        Ok(frame) => {
                            if tx.send(frame).await.is_err() {
                                break;
                            }
                        }
                        Err(e) => tracing::debug!(error = %e, "dropping malformed datagram"),
                    }
                }
                Ok(_) => {}
                Err(e) => {
                    tracing::debug!(error = %e, "client socket closed");
                    break;
                }
            }
        }
    });
    spawn_inner(
        socket,
        peer,
        rx,
        cfg,
        0,
        Role::Client,
        Some(ReaderAbort(reader_task)),
    )
}

/// Server-side channel for an allocated id; `inbox` is fed by the server's demux.
pub fn accept(
    socket: Arc<UdpSocket>,
    peer: SocketAddr,
    inbox: mpsc::Receiver<Frame>,
    cfg: ChannelConfig,
    conn_id: u32,
) -> Channel {
    spawn_inner(socket, peer, inbox, cfg, conn_id, Role::Server, None)
        .expect("spawn_inner is infallible")
}

fn spawn_inner(
    socket: Arc<UdpSocket>,
    peer: SocketAddr,
    inbox: mpsc::Receiver<Frame>,
    cfg: ChannelConfig,
    conn_id: u32,
    role: Role,
    reader: Option<ReaderAbort>,
) -> Result<Channel, ChannelError> {
    let (out_tx, mut out_rx) = mpsc::channel::<Vec<u8>>(256);
    let shared = Inbound::new(reader);
    // Held by the I/O task as well: the receive window is recomputed from it
    // every pass.
    let window_source = shared.clone();
    // Never block here: this task also answers ACKs and retransmits. See
    // `docs/notes/protocol-channel.md`.
    let (in_tx, in_rx) = mpsc::unbounded_channel::<Vec<u8>>();
    let conn_id_shared = Arc::new(std::sync::Mutex::new(conn_id));
    let conn_id_task = conn_id_shared.clone();

    let io = Sender { socket, peer };
    let mut inbox = inbox;
    let mut core = ChannelCore::new(cfg, role, conn_id);

    tokio::spawn(async move {
        let mut ticker = interval(core.cfg.tick);
        let mut out: Vec<Vec<u8>> = Vec::new();
        let mut result: Result<(), ChannelError> = Ok(());
        let mut syn_sent_at: Option<Instant> = None;

        if role == Role::Client {
            let syn = Frame::syn(0, core.cfg.window);
            result = io.send(&syn).await;
            syn_sent_at = Some(Instant::now());
            core.last_send = Instant::now();
        }

        'outer: while result.is_ok() {
            // Recomputed each pass from the backlog: a slow reader slows the sender.
            let window = core.advertised_window(window_source.backlog());
            core.recv_window = window;
            tokio::select! {
                incoming = inbox.recv() => {
                    match incoming {
                        Some(frame) => {
                            out.clear();
                            if let Err(e) = core.on_frame(frame, &mut out) {
                                result = Err(e);
                                break 'outer;
                            }
                            for msg in out.drain(..) {
                                if deliver(&in_tx, &window_source, msg).is_err() {
                                    result = Err(ChannelError::Closed);
                                    break 'outer;
                                }
                            }
                        }
                        // The demultiplexer or reader task went away.
                        None => break 'outer,
                    }
                }
                maybe_msg = out_rx.recv() => {
                    match maybe_msg {
                        Some(msg) => {
                            if let Err(e) = core.enqueue(msg) {
                                tracing::warn!(error = %e, "dropping oversized message");
                                continue;
                            }
                        }
                        None => break 'outer, // handle dropped: shut down cleanly
                    }
                }
                _ = ticker.tick() => {
                    if let Err(e) = on_tick(&mut core, &io, syn_sent_at, &mut out).await {
                        result = Err(e);
                        break 'outer;
                    }
                    for msg in out.drain(..) {
                        if deliver(&in_tx, &window_source, msg).is_err() {
                            result = Err(ChannelError::Closed);
                            break 'outer;
                        }
                    }
                }
            }
            if core.state == ConnState::Established && syn_sent_at.is_some() {
                syn_sent_at = None;
            }
            if let Err(e) = core.flush(&io).await {
                result = Err(e);
                break;
            }
        }

        if core.state != ConnState::Closing {
            let fin = Frame::fin(
                core.conn_id,
                core.next_seq,
                core.recv_expected,
                core.recv_window,
            );
            let _ = io.send(&fin).await;
        }
        *conn_id_task.lock().unwrap_or_else(|e| e.into_inner()) = core.conn_id;
        if let Err(e) = &result {
            tracing::debug!(%peer, error = %e, "channel finished");
        }
    });

    Ok(Channel {
        outbound: out_tx,
        inbound: in_rx,
        shared,
        peer,
        conn_id: conn_id_shared,
    })
}

/// Hands one message to the caller without ever waiting for it.
///
/// # Invariant
///
/// The message is counted before it is queued and uncounted when the caller takes
/// it, and that count is what the receive window is computed from.
fn deliver(
    tx: &mpsc::UnboundedSender<Vec<u8>>,
    backlog: &Arc<Inbound>,
    message: Vec<u8>,
) -> Result<(), ()> {
    backlog.pending.fetch_add(1, Ordering::Relaxed);
    if tx.send(message).is_err() {
        // Nobody is left to take it; the caller of the channel is gone.
        let _ = backlog
            .pending
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |n| n.checked_sub(1));
        return Err(());
    }
    Ok(())
}

async fn on_tick(
    core: &mut ChannelCore,
    io: &Sender,
    syn_sent_at: Option<Instant>,
    out: &mut Vec<Vec<u8>>,
) -> Result<(), ChannelError> {
    let now = Instant::now();

    if core.state == ConnState::Handshaking {
        let elapsed = syn_sent_at.map(|t| t.elapsed()).unwrap_or_default();
        if elapsed >= core.effective_rto() {
            if core.backoff >= core.cfg.max_retransmits {
                return Err(ChannelError::Timeout(core.backoff));
            }
            core.backoff += 1;
            io.send(&Frame::syn(0, core.cfg.window)).await?;
            core.last_send = now;
        }
        return Ok(());
    }
    let _ = out;

    // A (possibly repeated) SYN from the client always gets a SYN-ACK.
    if core.pending_syn_ack {
        core.pending_syn_ack = false;
        io.send(&Frame::syn_ack(core.conn_id, core.cfg.window))
            .await?;
        core.last_send = now;
    }

    // Keepalive.
    if now.duration_since(core.last_send) >= core.cfg.keepalive {
        core.send_ack(io).await?;
    }
    if now.duration_since(core.last_recv) >= core.cfg.idle_timeout {
        return Err(ChannelError::Timeout(core.backoff));
    }

    // Delayed ACK.
    if let Some(due) = core.ack_due {
        if now >= due {
            core.send_ack(io).await?;
        }
    }

    // Resend expired packets a few per tick; one at a time stalls a whole burst.
    let mut expired = Vec::new();
    for (&seq, pkt) in core.in_flight.iter() {
        if pkt.retransmits > core.cfg.max_retransmits {
            return Err(ChannelError::Timeout(pkt.retransmits));
        }
        if now.duration_since(pkt.sent_at) >= core.packet_rto(pkt.retransmits) {
            expired.push(seq);
            if expired.len() >= MAX_RETRANSMITS_PER_TICK {
                break;
            }
        }
    }
    for seq in expired {
        let Some(pkt) = core.in_flight.get_mut(&seq) else {
            continue;
        };
        pkt.retransmits += 1;
        pkt.retransmitted = true;
        pkt.sent_at = now;
        let frame = Frame::data(
            core.conn_id,
            seq,
            core.recv_expected,
            pkt.msg_id,
            pkt.frag_idx,
            pkt.frag_cnt,
            core.recv_window,
            pkt.payload.clone(),
        );
        io.send(&frame).await?;
        core.last_send = now;
    }

    // Fast retransmit requested by duplicate ACKs.
    if core.fast_retransmit {
        core.fast_retransmit = false;
        if let Some(pkt) = core.in_flight.get_mut(&core.send_base) {
            let seq = core.send_base;
            pkt.retransmits += 1;
            pkt.retransmitted = true;
            pkt.sent_at = now;
            let frame = Frame::data(
                core.conn_id,
                seq,
                core.recv_expected,
                pkt.msg_id,
                pkt.frag_idx,
                pkt.frag_cnt,
                core.recv_window,
                pkt.payload.clone(),
            );
            io.send(&frame).await?;
            core.last_send = now;
        }
    }

    Ok(())
}

/// How many stale packets one timer tick will resend.
const MAX_RETRANSMITS_PER_TICK: usize = 8;

/// Sequence-space comparison that survives 32-bit wraparound.
fn seq_lt(a: u32, b: u32) -> bool {
    (a.wrapping_sub(b) as i32) < 0
}

fn seq_le(a: u32, b: u32) -> bool {
    a == b || seq_lt(a, b)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_fin_ends_the_reading_side() {
        let mut core = ChannelCore::new(ChannelConfig::default(), Role::Server, 1);
        let mut out = Vec::new();
        let caught = core
            .on_frame(Frame::fin(1, 1, 0, 64), &mut out)
            .unwrap_err();
        assert!(matches!(caught, ChannelError::Closed), "{caught}");
        assert_eq!(core.state, ConnState::Closing);
    }

    #[test]
    fn sequence_comparison_wraps() {
        assert!(seq_lt(1, 2));
        assert!(!seq_lt(2, 1));
        // Across the wrap boundary.
        assert!(seq_lt(u32::MAX, 0));
        assert!(seq_le(5, 5));
    }

    #[test]
    fn fragments_cover_the_whole_message() {
        let cfg = ChannelConfig {
            mtu: 126, // 100-byte payloads
            ..ChannelConfig::default()
        };
        let mut core = ChannelCore::new(cfg, Role::Server, 1);
        let msg = vec![7u8; 250];
        core.enqueue(msg).unwrap();
        assert_eq!(core.queued.len(), 3);
        let total: usize = core.queued.iter().map(|q| q.payload.len()).sum();
        assert_eq!(total, 250);
        assert_eq!(core.queued[0].frag_idx, 0);
        assert_eq!(core.queued[2].frag_cnt, 3);
    }

    #[test]
    fn empty_message_still_yields_one_fragment() {
        let mut core = ChannelCore::new(ChannelConfig::default(), Role::Server, 1);
        core.enqueue(Vec::new()).unwrap();
        assert_eq!(core.queued.len(), 1);
    }

    #[test]
    fn reassembly_emits_once_all_fragments_arrive() {
        let mut core = ChannelCore::new(ChannelConfig::default(), Role::Server, 1);
        let mut out = Vec::new();
        core.accept(
            Frame::data(1, 1, 1, 9, 0, 2, 64, Bytes::from_static(b"ab")),
            &mut out,
        )
        .unwrap();
        assert!(out.is_empty());
        core.accept(
            Frame::data(1, 2, 1, 9, 1, 2, 64, Bytes::from_static(b"cd")),
            &mut out,
        )
        .unwrap();
        assert_eq!(out, vec![b"abcd".to_vec()]);
    }

    /// The advertised window is the room the caller has left, and never zero.
    /// See `docs/notes/protocol-channel.md`.
    #[test]
    fn the_advertised_window_shrinks_with_the_backlog_but_never_closes() {
        let cfg = ChannelConfig {
            window: 16,
            ..ChannelConfig::default()
        };
        let core = ChannelCore::new(cfg, Role::Server, 1);
        assert_eq!(core.advertised_window(0), 16);
        assert_eq!(core.advertised_window(4), 12);
        assert_eq!(core.advertised_window(16), 1);
        assert_eq!(core.advertised_window(1_000_000), 1);
    }

    #[test]
    fn rto_backoff_is_bounded() {
        let cfg = ChannelConfig::default();
        let mut core = ChannelCore::new(cfg.clone(), Role::Server, 1);
        core.backoff = 100;
        assert!(core.effective_rto() <= cfg.max_rto);
    }
}
